//! The statements of each boot initializer, run where Rails runs them.
//!
//! Rails `load`s every `config/initializers/**/*.rb` of every engine,
//! then of the app, sorted by full path (railties engine.rb
//! `load_config_initializers`), after the gems' railtie initializers and
//! before the Finisher. Each file runs top to bottom with `self` the
//! top-level `main`. Until this pass, every statement no reader modeled
//! was an `initializer_unsupported` gap and did not run.
//!
//! WHAT IS CARRIED. After every other reader (so only what none of them
//! consumed or refused), each top-level statement of each boot
//! initializer that passes [`admission`] is ingested as app code and
//! becomes one statement of `<holder>.run_initializer`, a singleton
//! method on a synthesized module, one per initializer
//! ([`App::initializer_programs`]). The analyzer types it like any app
//! method; the CRuby boot calls each holder in initializer order
//! (`project::initializer_program_block`).
//!
//! GUARDED. Each carried statement is wrapped in `begin … rescue
//! StandardError, LoadError, NotImplementedError` whose rescue names it
//! (`error[initializer_statement_not_run]: <file>:<line> <construct> did
//! not run: <class>: <message>`) and lets boot continue, the rule
//! `Rails::Railtie.not_run_unless_it_runs` applies to the gems' railtie
//! initializers. In Rails a raise there aborts boot; here it is counted
//! and reported where it happens, and the next statement runs.
//!
//! DEFERRED BLOCKS. A top-level call whose block Rails runs later and
//! whose value is ignored ([`DEFERRED`]: `ActiveSupport.on_load`,
//! `config.after_initialize`, `config.to_prepare`, …) is carried with the
//! admitted statements of its block, each guarded the same way; a
//! statement refused inside it is reported by itself, and the rest run.
//!
//! LOAD HOOKS. `ActiveSupport.on_load(:hook)` registers with the
//! runtime's port of lazy_load_hooks. It is carried only for a hook the
//! emitted app fires: a framework hook whose base the runtime defines
//! ([`FRAMEWORK_LOAD_HOOKS`], fired by boot as the runtime has loaded
//! that base), or a hook the app's own code fires
//! (`ActiveSupport.run_load_hooks(:hook, …)`, see [`class_load_hooks`]).
//! Inside the block `self` is the hook's base (`class_eval`); the
//! analyzer types it so.
//!
//! WHAT IS REFUSED, BY NAME. `def`, `class`, `module`; `self`, instance
//! and class variables of `main`; constant assignment below the top
//! level; `__FILE__`, `__dir__`, `require_relative` and `load` (the
//! initializer's own path); `return`/`yield`; calls with an implicit
//! receiver outside a load hook, other than [`KERNEL`]; `instance_eval`
//! and friends; Zeitwerk's `Rails.autoloaders.*.on_load` (the emitted
//! tree has no Zeitwerk loader); a `require` that is neither a runtime
//! port, a lib file boot already loads, nor a standard library feature;
//! and a statement that does not ingest as app code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ruby_prism::Node;

use crate::app::{App, ClassLoadHook, FrameworkLoadHook, InitializerProgram};
use crate::dialect::{LibraryClass, MethodDef};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::vfs::Vfs;

use super::util::{class_name_path, constant_id_str, constant_path_segments_strs, flatten_statements, module_name_path};

/// Framework load hooks the emitted runtime fires, the base Rails fires
/// each with, and where (rails 5813559f, 8.2.0.alpha). The runtime
/// defines each base before the app's initializers run.
pub const FRAMEWORK_LOAD_HOOKS: &[(&str, &str, &str)] = &[
    ("active_record", "ActiveRecord::Base", "activerecord/lib/active_record/base.rb:336"),
    ("action_controller_base", "ActionController::Base", "actionpack/lib/action_controller/base.rb:330"),
    ("action_controller", "ActionController::Base", "actionpack/lib/action_controller/base.rb:331"),
    ("active_job", "ActiveJob::Base", "activejob/lib/active_job/base.rb:76"),
    ("action_mailer", "ActionMailer::Base", "actionmailer/lib/action_mailer/base.rb:1083"),
];

/// Calls whose block Rails runs later, and whose value nobody reads: the
/// block's statements are carried one by one.
const DEFERRED: &[&str] =
    &["on_load", "after_initialize", "before_initialize", "to_prepare", "before_eager_load", "after_routes_loaded"];

/// Kernel methods a receiverless call may name at `main`.
const KERNEL: &[&str] = &[
    "raise", "fail", "puts", "warn", "print", "p", "pp", "lambda", "proc", "loop", "format", "sprintf", "Integer",
    "Float", "String", "Array", "Hash", "Rational", "sleep", "at_exit", "catch", "throw", "rand", "srand",
];

/// Standard library features: `require` loads them from Ruby itself.
const STDLIB: &[&str] = &[
    "base64", "benchmark", "bigdecimal", "csv", "date", "digest", "English", "erb", "etc", "fileutils", "forwardable",
    "ipaddr", "json", "logger", "monitor", "net/http", "objspace", "open3", "openssl", "ostruct", "pathname", "pp",
    "resolv", "securerandom", "set", "shellwords", "singleton", "socket", "stringio", "tempfile", "time", "timeout",
    "tmpdir", "uri", "yaml", "zlib",
];

/// Calls that run their block with another `self`.
const REBINDS_SELF: &[&str] =
    &["instance_eval", "instance_exec", "class_eval", "class_exec", "module_eval", "module_exec"];

/// The singleton method each holder carries.
pub const RUN_METHOD: &str = "run_initializer";

/// What the readers left of `files` and what fires load hooks: the
/// inputs of [`collect`].
pub(super) struct Inputs<'a, V: Vfs + ?Sized> {
    pub vfs: &'a V,
    pub dir: &'a Path,
    pub files: &'a [PathBuf],
    pub load_path: &'a [PathBuf],
    pub loaded_support: &'a crate::det::HashSet<PathBuf>,
    /// Hooks a runtime port lifts by itself (`on_load_hooks::FIRED_HOOKS`
    /// on PostgreSQL): left to that reader.
    pub lifted: &'a [&'a str],
}

/// The app's load hooks: those the app's own code fires (hook → base),
/// from [`class_load_hooks`] and every `ActiveSupport.run_load_hooks`
/// with a literal name anywhere in app code.
#[derive(Default)]
pub(super) struct FiredHooks {
    pub bases: BTreeMap<String, String>,
}

/// Every `ActiveSupport.run_load_hooks(<literal>, …)` in the app's
/// non-test, non-initializer sources. Those written in a class or module
/// body, or at a file's top level, are returned as [`ClassLoadHook`]s:
/// the emitted class file ends with them (`project`), so they fire when
/// the class loads, as in Rails. (Until this pass they were dropped
/// without a word: neither the model nor the library-class ingest carries
/// an explicit-receiver class-body call.) Those inside a method fire when
/// the method runs; their names count as fired.
pub(super) fn class_load_hooks(
    sources: &[crate::span::SourceFile],
    dir: &Path,
    skip: &[String],
) -> (Vec<ClassLoadHook>, FiredHooks) {
    let mut hooks = Vec::new();
    let mut fired = FiredHooks::default();
    for source in sources {
        if !source.path.ends_with(".rb") || skip.contains(&source.path) || !source.text.contains("run_load_hooks") {
            continue;
        }
        let rel = relative(dir, Path::new(&source.path));
        if rel.split('/').any(|seg| seg == "test" || seg == "spec") {
            continue;
        }
        let result = ruby_prism::parse(source.text.as_bytes());
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        let mut walk = SiteWalk { src: &source.text, rel: &rel, file: &source.path, hooks: &mut hooks, fired: &mut fired };
        walk.body(program.statements().body().iter().collect(), &[], None);
        // Inside methods and blocks: the name fires when that code runs.
        let mut deep = DeepSites { names: Vec::new() };
        ruby_prism::Visit::visit(&mut deep, &root);
        for (name, base) in deep.names {
            fired.bases.entry(name).or_insert(base);
        }
    }
    (hooks, fired)
}

struct SiteWalk<'a> {
    src: &'a str,
    rel: &'a str,
    file: &'a str,
    hooks: &'a mut Vec<ClassLoadHook>,
    fired: &'a mut FiredHooks,
}

impl SiteWalk<'_> {
    /// `nesting` is the enclosing class/module path; `last` the last
    /// top-level declaration of the file, for a file-level call.
    fn body(&mut self, stmts: Vec<Node<'_>>, nesting: &[String], mut last: Option<String>) {
        for stmt in stmts {
            if let Some(class) = stmt.as_class_node() {
                if let Some(path) = class_name_path(&class) {
                    let full: Vec<String> = nesting.iter().cloned().chain(path).collect();
                    last = Some(full.join("::"));
                    let inner = class.body().map(flatten_statements).unwrap_or_default();
                    self.body(inner, &full, None);
                }
                continue;
            }
            if let Some(module) = stmt.as_module_node() {
                if let Some(path) = module_name_path(&module) {
                    let full: Vec<String> = nesting.iter().cloned().chain(path).collect();
                    last = Some(full.join("::"));
                    let inner = module.body().map(flatten_statements).unwrap_or_default();
                    self.body(inner, &full, None);
                }
                continue;
            }
            let Some((name, literal, base)) = run_load_hooks_call(&stmt, self.src) else { continue };
            let owner = if nesting.is_empty() { last.clone() } else { Some(nesting.join("::")) };
            let base = match base {
                Base::Default => "Object".to_string(),
                Base::SelfRef => match &owner {
                    Some(o) if !nesting.is_empty() => o.clone(),
                    // `self` at a file's top level is `main`.
                    _ => continue,
                },
                Base::Const(text) => text,
                Base::Other => continue,
            };
            // A file-level call with an explicit base ends that class's file
            // when the file declares it; otherwise the file's last declaration.
            let class = if nesting.is_empty() && base != "Object" { Some(base.clone()) } else { owner };
            let Some(class) = class else { continue };
            let line = self.src[..stmt.location().start_offset()].matches('\n').count() + 1;
            self.fired.bases.insert(name.clone(), base.clone());
            let loc = stmt.location();
            self.hooks.push(ClassLoadHook {
                hook: name,
                hook_literal: literal,
                base,
                class,
                site: format!("{}:{line}", self.rel),
                span: crate::span::Span {
                    file: super::sources::file_id(self.file),
                    start: loc.start_offset() as u32,
                    end: loc.end_offset() as u32,
                },
            });
        }
    }
}

/// Every literal-named `ActiveSupport.run_load_hooks` inside a `def` or
/// a block, with its base when it is a constant.
struct DeepSites {
    names: Vec<(String, String)>,
}

impl<'pr> ruby_prism::Visit<'pr> for DeepSites {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if constant_id_str(&node.name()) == "run_load_hooks"
            && node.receiver().and_then(|r| constant_path_segments_strs(&r)).is_some_and(|p| p.join("::") == "ActiveSupport")
        {
            let args: Vec<Node<'pr>> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
            if let Some(name) = args.first().and_then(hook_name) {
                let base = args
                    .get(1)
                    .and_then(constant_path_segments_strs)
                    .map(|p| p.join("::"))
                    .unwrap_or_else(|| "Object".to_string());
                self.names.push((name.0, base));
            }
        }
        ruby_prism::visit_call_node(self, node);
    }
}

enum Base {
    Default,
    SelfRef,
    Const(String),
    Other,
}

/// `(name, literal)` of a Symbol or String hook name.
fn hook_name(node: &Node<'_>) -> Option<(String, String)> {
    if let Some(sym) = node.as_symbol_node() {
        let name = String::from_utf8_lossy(sym.unescaped()).into_owned();
        return Some((name.clone(), format!(":{name}")));
    }
    if let Some(s) = node.as_string_node() {
        let name = String::from_utf8_lossy(s.unescaped()).into_owned();
        return Some((name.clone(), format!("{name:?}")));
    }
    None
}

/// `ActiveSupport.run_load_hooks(<name>[, base])` as a statement.
fn run_load_hooks_call(stmt: &Node<'_>, _src: &str) -> Option<(String, String, Base)> {
    let call = stmt.as_call_node()?;
    if constant_id_str(&call.name()) != "run_load_hooks" || call.block().is_some() {
        return None;
    }
    if constant_path_segments_strs(&call.receiver()?)?.join("::") != "ActiveSupport" {
        return None;
    }
    let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
    let (name, literal) = hook_name(args.first()?)?;
    let base = match args.get(1) {
        None => Base::Default,
        Some(b) if b.as_self_node().is_some() => Base::SelfRef,
        Some(b) => match constant_path_segments_strs(b) {
            Some(p) => Base::Const(p.join("::")),
            None => Base::Other,
        },
    };
    match args.len() {
        1 | 2 => Some((name, literal, base)),
        _ => None,
    }
}

/// Carry every admitted statement of every initializer in `inputs.files`
/// (boot order) as a holder per file, consume what is carried and refuse
/// the rest by name. Returns the framework hooks the carried statements
/// register for.
pub(super) fn collect<V: Vfs + ?Sized>(app: &mut App, inputs: &Inputs<'_, V>, fired: &FiredHooks) {
    let mut used_names: crate::det::HashSet<String> =
        app.library_classes.iter().map(|lc| lc.name.0.as_str().to_string()).collect();
    let mut framework: BTreeMap<String, FrameworkLoadHook> = BTreeMap::new();
    for entry in inputs.files {
        let Ok(bytes) = inputs.vfs.read(entry) else { continue };
        let file = entry.display().to_string();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let rel = relative(inputs.dir, entry);
        let consumed = super::initializers::consumed(&file);
        let refused = super::initializers::refused_ranges(&file);
        let result = ruby_prism::parse(text.as_bytes());
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        let mut kept: Vec<Expr> = Vec::new();
        let mut carried = 0usize;
        for stmt in program.statements().body().iter() {
            let loc = stmt.location();
            let (start, end) = (loc.start_offset(), loc.end_offset());
            // A reader applied it, or part of it, or named why not.
            if consumed.iter().any(|&(a, b)| (a <= start && end <= b) || (start <= a && b <= end))
                || refused.iter().any(|&(a, b)| start <= a && b <= end)
                // Read after this pass (`content_helper_attribute_additions`).
                || text[start..end].contains("ActionText::ContentHelper.allowed_")
            {
                continue;
            }
            let scan = Scan::new(inputs, fired, &mut framework);
            let mut scan = scan;
            match scan.statement(&stmt) {
                Verdict::Lifted => continue,
                Verdict::Refused(reason) => {
                    super::initializers::refuse(&file, start, end, reason);
                }
                Verdict::Whole => match ingest(&stmt, &file) {
                    Ok(mut expr) => match rewrite_requires(&mut expr, inputs) {
                        Ok(()) => {
                            kept.push(guard(expr, &label(&rel, &text, &stmt)));
                            super::initializers::consume(&file, start, end);
                            carried += 1;
                        }
                        Err(reason) => super::initializers::refuse(&file, start, end, reason),
                    },
                    Err(reason) => super::initializers::refuse(&file, start, end, reason),
                },
                Verdict::Deferred { refused: inner_refused, kept: inner_kept } => {
                    if inner_kept.is_empty() {
                        // Every statement of the block is refused: the
                        // survey reports the call with the first reason.
                        if let Some((_, _, reason)) = inner_refused.first() {
                            super::initializers::refuse(&file, start, end, reason.clone());
                        }
                        continue;
                    }
                    match ingest(&stmt, &file) {
                        Ok(mut expr) => {
                            let refused_starts: Vec<u32> = inner_refused.iter().map(|(s, _, _)| *s as u32).collect();
                            let labels: BTreeMap<u32, String> = inner_kept
                                .iter()
                                .map(|&(s, e)| (s as u32, label_range(&rel, &text, s, e)))
                                .collect();
                            if let Err(reason) = rewrite_requires(&mut expr, inputs)
                                .and_then(|()| guard_block_body(&mut expr, &refused_starts, &labels))
                            {
                                super::initializers::refuse(&file, start, end, reason);
                                continue;
                            }
                            for (s, e, reason) in inner_refused {
                                super::initializers::refuse(&file, s, e, reason);
                            }
                            for &(s, e) in &inner_kept {
                                super::initializers::consume(&file, s, e);
                            }
                            carried += inner_kept.len();
                            // The call itself is the container the survey
                            // descends into; it runs as a whole, its
                            // registration guarded like any statement.
                            kept.push(guard(expr, &label(&rel, &text, &stmt)));
                        }
                        Err(reason) => super::initializers::refuse(&file, start, end, reason),
                    }
                }
            }
        }
        if kept.is_empty() {
            continue;
        }
        let holder = holder_name(&rel, &mut used_names);
        let Ok(mut synth) = crate::runtime_src::parse_methods(&format!("def self.{RUN_METHOD}\n  nil\nend\n")) else {
            continue;
        };
        let Some(mut method) = synth.pop() else { continue };
        let span = kept[0].span;
        method.body = Expr::new(span, ExprNode::Seq { exprs: kept });
        app.library_classes.push(holder_class(&holder, method));
        app.initializer_programs.push(InitializerProgram {
            file: rel,
            holder: ClassId(Symbol::from(holder.as_str())),
            statements: carried,
        });
    }
    app.framework_load_hooks = framework.into_values().collect();
}

pub(super) fn holder_class(name: &str, method: MethodDef) -> LibraryClass {
    LibraryClass {
        conditional_definitions: Vec::new(),
        top_level_constants: Vec::new(),
        name: ClassId(Symbol::from(name)),
        is_module: true,
        parent: None,
        parent_span: Default::default(),
        includes: Vec::new(),
        methods: vec![method],
        nullable_columns: Vec::new(),
        origin: None,
        constants: Vec::new(),
        unknown_calls: Vec::new(),
        class_ivar_initializers: Vec::new(),
        visibility_overrides: Vec::new(),
        method_aliases: Vec::new(),
    }
}

/// `RoundhouseInitializer` + the initializer's relative path, camelized.
fn holder_name(rel: &str, used: &mut crate::det::HashSet<String>) -> String {
    prefixed_holder_name("RoundhouseInitializer", rel, used)
}

/// `prefix` + `rel` camelized, unique among `used`.
pub(super) fn prefixed_holder_name(prefix: &str, rel: &str, used: &mut crate::det::HashSet<String>) -> String {
    let stem = rel.strip_suffix(".rb").unwrap_or(rel);
    let mut name = String::from(prefix);
    for part in stem.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
            name.extend(chars);
        }
    }
    let mut unique = name.clone();
    let mut n = 2;
    while !used.insert(unique.clone()) {
        unique = format!("{name}{n}");
        n += 1;
    }
    unique
}

pub(super) fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir).unwrap_or(path).display().to_string()
}

pub(super) fn label(rel: &str, text: &str, stmt: &Node<'_>) -> String {
    let loc = stmt.location();
    label_range(rel, text, loc.start_offset(), loc.end_offset())
}

pub(super) fn label_range(rel: &str, text: &str, start: usize, end: usize) -> String {
    let line = text[..start].matches('\n').count() + 1;
    let first = text[start..end].lines().next().unwrap_or("").trim();
    let mut short: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        short.push('…');
    }
    format!("{rel}:{line} {short}")
}

/// Ingest `stmt` as app code; a gap the survey would absorb into a `nil`
/// placeholder refuses it instead.
pub(super) fn ingest(stmt: &Node<'_>, file: &str) -> Result<Expr, String> {
    let before = super::survey::recorded_len();
    match super::expr::ingest_expr(stmt, file) {
        Ok(expr) => match super::survey::recorded_since(before) {
            Some(gap) => Err(format!("does not ingest as app code: {gap}")),
            None => Ok(expr),
        },
        Err(err) => Err(format!("does not ingest as app code: {err}")),
    }
}

/// The guard around one carried statement.
pub(super) fn guard(stmt: Expr, label: &str) -> Expr {
    let site: String = label.chars().map(|c| if matches!(c, '"' | '\\' | '#') { '\'' } else { c }).collect();
    let template = format!(
        "def __guard\n  begin\n    nil\n  rescue StandardError, LoadError, NotImplementedError => __initializer_error\n    \
         warn \"roundhouse: error[initializer_statement_not_run]: {site} did not run: #{{__initializer_error.class}}: #{{__initializer_error.message}}\"\n  \
         end\nend\n"
    );
    let Ok(mut methods) = crate::runtime_src::parse_methods(&template) else { return stmt };
    let Some(method) = methods.pop() else { return stmt };
    let mut begin = method.body;
    let single = match &mut *begin.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => exprs.pop(),
        _ => None,
    };
    if let Some(only) = single {
        begin = only;
    }
    let span = stmt.span;
    match &mut *begin.node {
        ExprNode::BeginRescue { body, .. } => *body = stmt,
        _ => return stmt,
    }
    begin.span = span;
    begin
}

/// Drop the refused statements of the deferred call's block and guard
/// the rest.
pub(super) fn guard_block_body(expr: &mut Expr, refused: &[u32], labels: &BTreeMap<u32, String>) -> Result<(), String> {
    let ExprNode::Send { block: Some(block), .. } = &mut *expr.node else {
        return Err("the deferred call's block did not ingest as a block".into());
    };
    let ExprNode::Lambda { body, .. } = &mut *block.node else {
        return Err("the deferred call's block did not ingest as a block".into());
    };
    let stmts: Vec<Expr> = match &*body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![body.clone()],
    };
    let mut kept = Vec::new();
    for s in stmts {
        if refused.contains(&s.span.start) {
            continue;
        }
        let label = labels.get(&s.span.start).cloned().unwrap_or_else(|| "initializer statement".into());
        kept.push(guard(s, &label));
    }
    let span = body.span;
    *body = Expr::new(span, ExprNode::Seq { exprs: kept });
    Ok(())
}

/// `require "x"` → a runtime port, a no-op for a lib file boot already
/// loads, or the standard library feature itself; anything else refuses.
pub(super) fn rewrite_requires<V: Vfs + ?Sized>(expr: &mut Expr, inputs: &Inputs<'_, V>) -> Result<(), String> {
    let mut failure: Option<String> = None;
    walk(expr, &mut |e| {
        if failure.is_some() {
            return;
        }
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &mut *e.node else { return };
        if method.as_str() != "require" {
            return;
        }
        let feature = match args.as_slice() {
            [arg] => match &*arg.node {
                ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
                _ => {
                    failure = Some("`require` of a computed name".into());
                    return;
                }
            },
            _ => {
                failure = Some("`require` with other than one argument".into());
                return;
            }
        };
        if let Some((_, stem)) = super::on_load_hooks::RUNTIME_PORTS.iter().find(|(name, _)| *name == feature) {
            // The holder is emitted at app/models/<holder>.rb.
            *method = Symbol::from("require_relative");
            if let ExprNode::Lit { value: Literal::Str { value } } = &mut *args[0].node {
                *value = format!("../../runtime/{stem}");
            }
            return;
        }
        let file = if feature.ends_with(".rb") { feature.clone() } else { format!("{feature}.rb") };
        let target: Option<PathBuf> = inputs.load_path.iter().map(|lib| lib.join(&file)).find(|p| inputs.vfs.exists(p));
        if target.as_ref().is_some_and(|t| inputs.loaded_support.contains(t)) {
            *e.node = ExprNode::Lit { value: Literal::Nil };
            return;
        }
        if target.is_none() && STDLIB.contains(&feature.as_str()) {
            return;
        }
        failure = Some(match target {
            Some(t) => format!("`require {feature:?}` loads {}, which boot does not load", relative(inputs.dir, &t)),
            None => format!("`require {feature:?}`: not a runtime port, a lib file of the tree or a stdlib feature"),
        });
    });
    match failure {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

pub(super) fn walk(expr: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(expr);
    expr.node.for_each_child_mut(&mut |child| walk(child, f));
}

pub(super) enum Verdict {
    /// Another reader lifts it (an adapter hook on PostgreSQL).
    Lifted,
    Refused(String),
    Whole,
    /// A deferred call: its block's refused `(start, end, reason)` and
    /// kept `(start, end)` statements.
    Deferred { refused: Vec<(usize, usize, String)>, kept: Vec<(usize, usize)> },
}

/// What an environment file's statement may do beyond an initializer's
/// (`ingest::environments`).
pub(super) struct EnvironmentRules<'s> {
    /// A statement of `Rails.application.configure do … end`, whose
    /// `self` is the application: a bare `config` is its config.
    pub in_configure: bool,
    /// Each `config.*` write (a call or an operator write on an anchored
    /// config chain) found in the statement: `Some(reason)` refuses it.
    pub config_write: &'s dyn Fn(&Node<'_>) -> Option<String>,
}

/// The admission rule (module doc), statement by statement.
pub(super) struct Scan<'s, 'a, V: Vfs + ?Sized> {
    inputs: &'s Inputs<'a, V>,
    fired: &'s FiredHooks,
    framework: &'s mut BTreeMap<String, FrameworkLoadHook>,
    /// Inside an `on_load` block: `self` is the hook's base.
    in_hook: usize,
    reason: Option<String>,
    /// An environment file's statement, not an initializer's.
    env: Option<EnvironmentRules<'s>>,
}

impl<'s, 'a, V: Vfs + ?Sized> Scan<'s, 'a, V> {
    fn new(inputs: &'s Inputs<'a, V>, fired: &'s FiredHooks, framework: &'s mut BTreeMap<String, FrameworkLoadHook>) -> Self {
        Scan { inputs, fired, framework, in_hook: 0, reason: None, env: None }
    }

    /// The admission rule for an environment file's statement.
    pub(super) fn environment(
        inputs: &'s Inputs<'a, V>,
        fired: &'s FiredHooks,
        framework: &'s mut BTreeMap<String, FrameworkLoadHook>,
        rules: EnvironmentRules<'s>,
    ) -> Self {
        Scan { inputs, fired, framework, in_hook: 0, reason: None, env: Some(rules) }
    }

    pub(super) fn statement(&mut self, stmt: &Node<'_>) -> Verdict {
        if let Some(call) = stmt.as_call_node() {
            let name = constant_id_str(&call.name());
            if let Some(block) = call.block().and_then(|b| b.as_block_node()) {
                if DEFERRED.contains(&name) {
                    // The call minus its block.
                    if let Some(r) = call.receiver() {
                        self.visit(&r);
                    }
                    if let Some(args) = call.arguments() {
                        for a in args.arguments().iter() {
                            self.visit(&a);
                        }
                    }
                    let hook = name == "on_load";
                    if hook {
                        match self.hook(&call) {
                            Ok(true) => {}
                            Ok(false) => return Verdict::Lifted,
                            Err(reason) => return Verdict::Refused(reason),
                        }
                    }
                    if let Some(reason) = self.reason.take() {
                        return Verdict::Refused(reason);
                    }
                    let body = block.body().map(flatten_statements).unwrap_or_default();
                    let (mut refused, mut kept) = (Vec::new(), Vec::new());
                    for inner in body {
                        let loc = inner.location();
                        if hook {
                            self.in_hook += 1;
                        }
                        self.visit(&inner);
                        if hook {
                            self.in_hook -= 1;
                        }
                        match self.reason.take() {
                            Some(reason) => refused.push((loc.start_offset(), loc.end_offset(), reason)),
                            None => kept.push((loc.start_offset(), loc.end_offset())),
                        }
                    }
                    return Verdict::Deferred { refused, kept };
                }
            }
        }
        self.visit(stmt);
        match self.reason.take() {
            Some(reason) => Verdict::Refused(reason),
            None => Verdict::Whole,
        }
    }

    fn visit(&mut self, node: &Node<'_>) {
        let mut v = ScanVisit { scan: self };
        ruby_prism::Visit::visit(&mut v, node);
    }

    fn refuse(&mut self, reason: impl Into<String>) {
        if self.reason.is_none() {
            self.reason = Some(reason.into());
        }
    }

    /// `Ok(true)`: an `on_load` the emitted app fires; `Ok(false)`: one a
    /// runtime port lifts itself; `Err`: why it cannot run.
    fn hook(&mut self, call: &ruby_prism::CallNode<'_>) -> Result<bool, String> {
        let receiver = call.receiver().ok_or("a receiverless `on_load`")?;
        let path = constant_path_segments_strs(&receiver).map(|p| p.join("::"));
        if path.as_deref() != Some("ActiveSupport") {
            let loc = receiver.location();
            let text = String::from_utf8_lossy(loc.as_slice());
            if text.contains("autoloaders") {
                return Err("Zeitwerk `on_load` runs when its loader autoloads the constant; the emitted tree has \
                            no Zeitwerk loader"
                    .into());
            }
            return Err(format!("`on_load` on `{}` is not ActiveSupport's load hook", cap(&text, 40)));
        }
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let Some((name, _)) = args.first().and_then(hook_name) else {
            return Err("`on_load` with a computed hook name".into());
        };
        if self.inputs.lifted.contains(&name.as_str()) {
            return Ok(false);
        }
        if let Some((hook, base, site)) = FRAMEWORK_LOAD_HOOKS.iter().find(|(h, _, _)| *h == name) {
            self.framework.entry(name.clone()).or_insert_with(|| FrameworkLoadHook {
                hook: hook.to_string(),
                base: base.to_string(),
                site: site.to_string(),
            });
            return Ok(true);
        }
        if self.fired.bases.contains_key(&name) {
            return Ok(true);
        }
        Err(format!(
            "nothing fires `:{name}`: no `ActiveSupport.run_load_hooks(:{name}, …)` in the runtime or the app's code"
        ))
    }
}

/// The base `on_load(:hook)`'s block runs on (`class_eval`), for the
/// analyzer: a framework hook's, or the one the app's code fires it with.
pub fn hook_base(app: &App, hook: &str) -> Option<String> {
    if let Some((_, base, _)) = FRAMEWORK_LOAD_HOOKS.iter().find(|(h, _, _)| *h == hook) {
        return Some(base.to_string());
    }
    app.load_hook_bases.get(hook).cloned()
}

fn cap(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

struct ScanVisit<'v, 's, 'a, V: Vfs + ?Sized> {
    scan: &'v mut Scan<'s, 'a, V>,
}

impl<V: Vfs + ?Sized> ScanVisit<'_, '_, '_, V> {
    /// An environment file's refused `config.*` operator write: refuses
    /// the statement.
    fn env_config_write(&mut self, node: &Node<'_>) -> bool {
        if self.scan.reason.is_some() {
            return true;
        }
        let Some(rules) = &self.scan.env else { return false };
        match (rules.config_write)(node) {
            Some(reason) => {
                self.scan.refuse(reason);
                true
            }
            None => false,
        }
    }
}

impl<'pr, V: Vfs + ?Sized> ruby_prism::Visit<'pr> for ScanVisit<'_, '_, '_, V> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if self.scan.reason.is_some() {
            return;
        }
        let name = constant_id_str(&node.name());
        if let Some(rules) = &self.scan.env {
            if let Some(reason) = (rules.config_write)(&node.as_node()) {
                self.scan.refuse(reason);
                return;
            }
        }
        if REBINDS_SELF.contains(&name) {
            self.scan.refuse(format!("`{name}` runs its block with another `self`"));
            return;
        }
        if name == "on_load" && node.block().is_some() {
            match self.scan.hook(node) {
                Ok(true) => {}
                Ok(false) => {
                    self.scan.refuse("an adapter load hook inside another statement");
                    return;
                }
                Err(reason) => {
                    self.scan.refuse(reason);
                    return;
                }
            }
            if let Some(r) = node.receiver() {
                self.visit(&r);
            }
            if let Some(args) = node.arguments() {
                for a in args.arguments().iter() {
                    self.visit(&a);
                }
            }
            if let Some(block) = node.block() {
                self.scan.in_hook += 1;
                self.visit(&block);
                self.scan.in_hook -= 1;
            }
            return;
        }
        if node.receiver().is_none() {
            let env = self.scan.env.as_ref().map(|rules| rules.in_configure);
            match name {
                "require" => {}
                // The environment file's own path: resolved by
                // `ingest::environments`.
                "require_relative" if env.is_some() && node.block().is_none() => {}
                // `self` of `Rails.application.configure` is the
                // application; its `config` is `Rails.application.config`.
                "config" if env == Some(true) && node.arguments().is_none() && node.block().is_none() => {}
                _ if env == Some(true) && self.scan.in_hook == 0 && !KERNEL.contains(&name) => {
                    self.scan.refuse(format!(
                        "`{name}` has an implicit receiver, Rails.application (the `configure` block's `self`), \
                         whose `{name}` the emitted boot does not carry"
                    ));
                    return;
                }
                "require_relative" | "load" | "autoload" | "__dir__" | "binding" => {
                    self.scan.refuse(format!("`{name}` reads the initializer's own file or scope"));
                    return;
                }
                "include" | "extend" | "prepend" if self.scan.in_hook == 0 => {
                    self.scan.refuse(format!("`{name}` at the top level mixes into Object"));
                    return;
                }
                _ if self.scan.in_hook > 0 => {}
                _ if KERNEL.contains(&name) => {}
                _ => {
                    self.scan.refuse(format!("`{name}` has an implicit receiver, `main`, which the emitted boot does not carry"));
                    return;
                }
            }
        }
        ruby_prism::visit_call_node(self, node);
    }
    fn visit_call_operator_write_node(&mut self, node: &ruby_prism::CallOperatorWriteNode<'pr>) {
        if self.env_config_write(&node.as_node()) {
            return;
        }
        ruby_prism::visit_call_operator_write_node(self, node);
    }
    fn visit_call_or_write_node(&mut self, node: &ruby_prism::CallOrWriteNode<'pr>) {
        if self.env_config_write(&node.as_node()) {
            return;
        }
        ruby_prism::visit_call_or_write_node(self, node);
    }
    fn visit_call_and_write_node(&mut self, node: &ruby_prism::CallAndWriteNode<'pr>) {
        if self.env_config_write(&node.as_node()) {
            return;
        }
        ruby_prism::visit_call_and_write_node(self, node);
    }
    fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {
        self.scan.refuse("`def` in an initializer defines a method on Object (or the hook's base)");
    }
    fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {
        self.scan.refuse("a `class` body in an initializer");
    }
    fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {
        self.scan.refuse("a `module` body in an initializer");
    }
    fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {
        self.scan.refuse("a `class <<` body in an initializer");
    }
    fn visit_self_node(&mut self, _: &ruby_prism::SelfNode<'pr>) {
        if self.scan.in_hook == 0 {
            self.scan.refuse("`self` at an initializer's top level is `main`");
        }
    }
    fn visit_instance_variable_read_node(&mut self, _: &ruby_prism::InstanceVariableReadNode<'pr>) {
        self.scan.refuse("an instance variable of `main` (or of the hook's base)");
    }
    fn visit_instance_variable_write_node(&mut self, _: &ruby_prism::InstanceVariableWriteNode<'pr>) {
        self.scan.refuse("an instance variable of `main` (or of the hook's base)");
    }
    fn visit_instance_variable_or_write_node(&mut self, _: &ruby_prism::InstanceVariableOrWriteNode<'pr>) {
        self.scan.refuse("an instance variable of `main` (or of the hook's base)");
    }
    fn visit_class_variable_read_node(&mut self, _: &ruby_prism::ClassVariableReadNode<'pr>) {
        self.scan.refuse("a class variable in an initializer");
    }
    fn visit_class_variable_write_node(&mut self, _: &ruby_prism::ClassVariableWriteNode<'pr>) {
        self.scan.refuse("a class variable in an initializer");
    }
    fn visit_source_file_node(&mut self, _: &ruby_prism::SourceFileNode<'pr>) {
        self.scan.refuse("`__FILE__` is the initializer's own path");
    }
    fn visit_return_node(&mut self, _: &ruby_prism::ReturnNode<'pr>) {
        self.scan.refuse("`return` in an initializer");
    }
    fn visit_yield_node(&mut self, _: &ruby_prism::YieldNode<'pr>) {
        self.scan.refuse("`yield` in an initializer");
    }
    fn visit_constant_write_node(&mut self, _: &ruby_prism::ConstantWriteNode<'pr>) {
        self.scan.refuse("a constant assigned below the initializer's top level");
    }
    fn visit_constant_path_write_node(&mut self, _: &ruby_prism::ConstantPathWriteNode<'pr>) {
        self.scan.refuse("a constant assigned below the initializer's top level");
    }
    fn visit_constant_or_write_node(&mut self, _: &ruby_prism::ConstantOrWriteNode<'pr>) {
        self.scan.refuse("a constant assigned below the initializer's top level");
    }
    fn visit_constant_path_or_write_node(&mut self, _: &ruby_prism::ConstantPathOrWriteNode<'pr>) {
        self.scan.refuse("a constant assigned below the initializer's top level");
    }
    fn visit_alias_method_node(&mut self, _: &ruby_prism::AliasMethodNode<'pr>) {
        self.scan.refuse("`alias` in an initializer");
    }
    fn visit_undef_node(&mut self, _: &ruby_prism::UndefNode<'pr>) {
        self.scan.refuse("`undef` in an initializer");
    }
    fn visit_super_node(&mut self, _: &ruby_prism::SuperNode<'pr>) {
        self.scan.refuse("`super` in an initializer");
    }
    fn visit_forwarding_super_node(&mut self, _: &ruby_prism::ForwardingSuperNode<'pr>) {
        self.scan.refuse("`super` in an initializer");
    }
    fn visit_x_string_node(&mut self, _: &ruby_prism::XStringNode<'pr>) {
        self.scan.refuse("a shell command in an initializer");
    }
    fn visit_interpolated_x_string_node(&mut self, _: &ruby_prism::InterpolatedXStringNode<'pr>) {
        self.scan.refuse("a shell command in an initializer");
    }
}
