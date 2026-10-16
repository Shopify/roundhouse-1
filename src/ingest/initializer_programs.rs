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
//! that base), a test-case hook the emitted test helper fires
//! ([`TEST_LOAD_HOOKS`], where rails/test_help loads each test base), or
//! a hook the app's own code fires (`ActiveSupport.run_load_hooks(:hook,
//! …)`, see [`class_load_hooks`]).
//! Inside the block `self` is the hook's base (`class_eval`); the
//! analyzer types it so.
//!
//! DEFINITIONS. A top-level `module`/`class` the tree carries as a
//! library class (`app::keep_initializer_defined`: one the app names,
//! or one the file mixes in) is a read of its constant at its place in
//! the program, which autoloads its emitted file: it is defined where
//! Rails runs the definition.
//!
//! WHAT IS REFUSED, BY NAME. `def`, any other `class` or `module`; `self`, instance
//! and class variables of `main`; constant assignment below the top
//! level; `__FILE__`, `__dir__`, `require_relative` and `load` (the
//! initializer's own path); `return`/`yield`; calls with an implicit
//! receiver outside a load hook, other than [`KERNEL`]; `instance_eval`
//! and friends; Zeitwerk's `on_load` without a literal constant path,
//! or on a loader other than `Rails.autoloaders.main`/`.once` (the
//! emitted tree reports the load only of a constant a literal path
//! names, runtime/spinel/rails_autoloaders.rb); a `require` that is neither a runtime
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

/// The test-case load hooks the emitted test helper fires (the PostgreSQL
/// CRuby tree, `project::test_case_load_hooks_block`), each on the base
/// Rails fires it with, where (rails 5813559f, 8.2.0.alpha). Each base's
/// file ends firing its hooks as it loads, after boot:
/// active_support/test_case where rails/test_help.rb requires it, the
/// others where a test first names them (autoloads), which the emitted
/// helper takes as the first class descending from one. A hook
/// registered later (rails/test_help's own, a test-support file's) runs
/// at once. Boot does not fire them: an initializer's or engine's
/// `on_load(:active_support_test_case)` waits for the test helper, as
/// natively it waits for the test environment.
pub const TEST_LOAD_HOOKS: &[(&str, &str, &str)] = &[
    ("active_support_test_case", "ActiveSupport::TestCase", "activesupport/lib/active_support/test_case.rb:408"),
    ("action_controller_test_case", "ActionController::TestCase", "actionpack/lib/action_controller/test_case.rb:611"),
    (
        "action_dispatch_integration_test",
        "ActionDispatch::IntegrationTest",
        "actionpack/lib/action_dispatch/testing/integration.rb:693",
    ),
    ("action_view_test_case", "ActionView::TestCase", "actionview/lib/action_view/test_case.rb:256"),
    ("active_job_test_case", "ActiveJob::TestCase", "activejob/lib/active_job/test_case.rb:9"),
];

/// The [`TEST_LOAD_HOOKS`] Rails fires in its base's `Behavior`'s
/// `included do`, with the includer, rather than in the base's class
/// body: actionpack test_case.rb:611 (ActionController::TestCase::Behavior),
/// testing/integration.rb:693 (ActionDispatch::IntegrationTest::Behavior),
/// actionview test_case.rb:256 (ActionView::TestCase::Behavior). A test
/// that includes `Behavior` itself (Core's
/// components/developer_dashboard check_box_test.rb `include
/// ActionView::TestCase::Behavior`) gets the hooks run on its own class.
pub const TEST_BEHAVIOR_HOOKS: &[&str] = &["action_controller_test_case", "action_dispatch_integration_test", "action_view_test_case"];

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
    /// The test load path (`App::test_load_roots`): a feature found only
    /// there is left as written, for the emitted `require` to resolve
    /// (`project::test_load_path_features`).
    pub test_load_path: &'a [PathBuf],
    pub loaded_support: &'a crate::det::HashSet<PathBuf>,
    /// The files of a gem compiled from source (`App::gem_source_dirs`)
    /// the tree carries, as a file program or declarations, relative to
    /// the app root: a `require` of one is left as written, for the
    /// emitted tree's compiled-gem `require` to load
    /// (`project::compiled_gem_features`).
    pub gem_files: &'a crate::det::HashSet<String>,
    /// The file programs (`App::file_programs`) of the other `lib/`
    /// directories on the load path (the app's, a path gem's), relative
    /// to the app root: a `require` of one loads its emitted program.
    pub lib_programs: &'a crate::det::HashSet<String>,
    /// Hooks a runtime port lifts by itself (`on_load_hooks::FIRED_HOOKS`
    /// on PostgreSQL): left to that reader.
    pub lifted: &'a [&'a str],
    /// The gem home and lock the tree's installed gems resolve in: a
    /// `require` of a file one of them owns is kept, for the emitted
    /// program to load it as Ruby does.
    pub gems: Option<(&'a Path, &'a crate::gems::Lockfile)>,
}

/// The app's load hooks: those the app's own code fires (hook → base),
/// from [`class_load_hooks`] and every `ActiveSupport.run_load_hooks`
/// with a literal name anywhere in app code.
#[derive(Default)]
pub(super) struct FiredHooks {
    pub bases: BTreeMap<String, String>,
    /// The modules that `extend GraphQL::Shopify::ConfigureType`, by full
    /// name ([`configure_modules`]): each `configure_type` on one defines
    /// a `configure_<type>` singleton method that registers its block with
    /// `ActiveSupport.on_load`, which runs it with `self` the type.
    pub configure_modules: crate::det::HashSet<String>,
}

/// The modules `extend GraphQL::Shopify::ConfigureType` in the app's and
/// the compiled gems' non-test sources (graphql-shopify
/// lib/graphql/shopify/configure_type.rb), by full name.
pub(super) fn configure_modules(sources: &[crate::span::SourceFile], dir: &Path) -> crate::det::HashSet<String> {
    fn walk(stmts: Vec<Node<'_>>, nesting: &[String], out: &mut crate::det::HashSet<String>) {
        for stmt in stmts {
            if let Some(call) = stmt.as_call_node() {
                let extends = call.receiver().is_none()
                    && constant_id_str(&call.name()) == "extend"
                    && call.arguments().is_some_and(|a| {
                        a.arguments().iter().any(|arg| {
                            constant_path_segments_strs(&arg)
                                .is_some_and(|p| p.join("::").trim_start_matches("::") == "GraphQL::Shopify::ConfigureType")
                        })
                    });
                if extends && !nesting.is_empty() {
                    out.insert(nesting.join("::"));
                }
                continue;
            }
            let (path, body) = if let Some(class) = stmt.as_class_node() {
                (class_name_path(&class), class.body())
            } else if let Some(module) = stmt.as_module_node() {
                (module_name_path(&module), module.body())
            } else {
                continue;
            };
            let Some(path) = path else { continue };
            let full: Vec<String> = nesting.iter().cloned().chain(path).collect();
            walk(body.map(flatten_statements).unwrap_or_default(), &full, out);
        }
    }
    let mut out = crate::det::HashSet::default();
    for source in sources {
        if !source.path.ends_with(".rb") || !source.text.contains("GraphQL::Shopify::ConfigureType") {
            continue;
        }
        let rel = relative(dir, Path::new(&source.path));
        if rel.split('/').any(|seg| seg == "test" || seg == "spec") {
            continue;
        }
        let result = ruby_prism::parse(source.text.as_bytes());
        if let Some(program) = result.node().as_program_node() {
            walk(program.statements().body().iter().collect(), &[], &mut out);
        }
    }
    out
}

/// Every `ActiveSupport.run_load_hooks(<literal>, …)` in the app's
/// non-test, non-initializer sources. Those written in a class or module
/// body, or at a file's top level, are returned as [`ClassLoadHook`]s:
/// the emitted class file ends with them (`project`), so they fire when
/// the class loads, as in Rails. (Until this pass they were dropped
/// without a word: neither the model nor the library-class ingest carries
/// an explicit-receiver class-body call.) Those inside a method fire when
/// the method runs; their names count as fired.
///
/// A relative constant base resolves as Ruby's lexical lookup would at
/// the call, against the scopes enclosing it: activeresource 6.2.0's
/// base.rb ends `module ActiveResource` with
/// `ActiveSupport.run_load_hooks(:active_resource, Base)`, and the call
/// the emitted file runs outside that body names `ActiveResource::Base`.
/// A scope's constant counts when this file or `known` (the app's
/// classes and modules) declares it; one neither declares stays as
/// written, for the top level.
pub(super) fn class_load_hooks(
    sources: &[crate::span::SourceFile],
    dir: &Path,
    skip: &[String],
    known: &crate::det::HashSet<String>,
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
        let mut declared = crate::det::HashSet::default();
        declarations(program.statements().body().iter().collect(), &[], &mut declared);
        let mut walk = SiteWalk {
            src: &source.text,
            rel: &rel,
            file: &source.path,
            declared: &declared,
            known,
            hooks: &mut hooks,
            fired: &mut fired,
        };
        walk.body(program.statements().body().iter().collect(), &[], &[], None);
        // Inside methods and blocks: the name fires when that code runs.
        let mut deep = DeepSites { names: Vec::new() };
        ruby_prism::Visit::visit(&mut deep, &root);
        for (name, base) in deep.names {
            fired.bases.entry(name).or_insert(base);
        }
    }
    (hooks, fired)
}

/// Every class and module `stmts` declare, nested ones too, by full name.
fn declarations(stmts: Vec<Node<'_>>, nesting: &[String], out: &mut crate::det::HashSet<String>) {
    for stmt in stmts {
        let (path, body) = if let Some(class) = stmt.as_class_node() {
            (class_name_path(&class), class.body())
        } else if let Some(module) = stmt.as_module_node() {
            (module_name_path(&module), module.body())
        } else {
            continue;
        };
        let Some(path) = path else { continue };
        let full: Vec<String> = nesting.iter().cloned().chain(path).collect();
        out.insert(full.join("::"));
        declarations(body.map(flatten_statements).unwrap_or_default(), &full, out);
    }
}

struct SiteWalk<'a> {
    src: &'a str,
    rel: &'a str,
    file: &'a str,
    /// The classes and modules this file declares ([`declarations`]).
    declared: &'a crate::det::HashSet<String>,
    known: &'a crate::det::HashSet<String>,
    hooks: &'a mut Vec<ClassLoadHook>,
    fired: &'a mut FiredHooks,
}

impl SiteWalk<'_> {
    /// The class a call in the body of namespace `nesting` ends: the
    /// namespace, unless the file is the one Zeitwerk loads for a
    /// declaration this body wrote before the call (`last`). Core's
    /// optimization/rollouts/changeset_registry.rb ends `module Rollouts`
    /// with `ActiveSupport.run_load_hooks(:rollout_changesets_registry,
    /// Optimization::Rollouts)` after `module ChangesetRegistry`: natively
    /// it fires when ChangesetRegistry's file loads. Ending the namespace's
    /// file, it fired at the first `Optimization::Rollouts::…` reference
    /// (another class body's include), and the hook's on_load block then
    /// loaded the file still loading: a circular require.
    fn namespace_owner(&self, nesting: &[String], last: Option<&str>) -> String {
        let namespace = nesting.join("::");
        let stem = self.rel.strip_suffix(".rb").unwrap_or(self.rel);
        let names_file = |name: &str| {
            let path = crate::naming::underscore(name);
            stem == path || stem.ends_with(&format!("/{path}"))
        };
        match last {
            Some(last) if !names_file(&namespace) && names_file(last) => last.to_string(),
            _ => namespace,
        }
    }

    /// The constant a relative `text` names at a call in `scopes` (the
    /// lexical scopes, outermost first, by full name): the innermost
    /// scope's that the file or the app declares, else `text`.
    fn resolve(&self, text: String, scopes: &[String]) -> String {
        scopes
            .iter()
            .rev()
            .map(|scope| format!("{scope}::{text}"))
            .find(|full| self.declared.contains(full) || self.known.contains(full))
            .unwrap_or(text)
    }

    /// `nesting` is the enclosing class/module path; `scopes` the lexical
    /// scopes (`module A::B` opens one, `A::B`); `last` the last
    /// top-level declaration of the file, for a file-level call.
    fn body(&mut self, stmts: Vec<Node<'_>>, nesting: &[String], scopes: &[String], mut last: Option<String>) {
        for stmt in stmts {
            let declaration = if let Some(class) = stmt.as_class_node() {
                Some((class_name_path(&class), class.body()))
            } else {
                stmt.as_module_node().map(|module| (module_name_path(&module), module.body()))
            };
            if let Some((path, inner)) = declaration {
                if let Some(path) = path {
                    let full: Vec<String> = nesting.iter().cloned().chain(path).collect();
                    let name = full.join("::");
                    last = Some(name.clone());
                    let scopes: Vec<String> = scopes.iter().cloned().chain(std::iter::once(name)).collect();
                    let inner = inner.map(flatten_statements).unwrap_or_default();
                    self.body(inner, &full, &scopes, None);
                }
                continue;
            }
            let Some((name, literal, base)) = run_load_hooks_call(&stmt, self.src) else { continue };
            let owner = if nesting.is_empty() { last.clone() } else { Some(self.namespace_owner(nesting, last.as_deref())) };
            let base = match base {
                Base::Default => "Object".to_string(),
                // `self` in a namespace's body is the namespace, whichever
                // file the call ends.
                Base::SelfRef => match &owner {
                    Some(_) if !nesting.is_empty() => nesting.join("::"),
                    // `self` at a file's top level is `main`.
                    _ => continue,
                },
                Base::Const(text) => self.resolve(text, scopes),
                Base::Absolute(text) => text,
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

/// The app's classes and modules by full name, for
/// [`class_load_hooks`] to resolve a relative base against.
pub(super) fn known_constants(app: &App) -> crate::det::HashSet<String> {
    app.models
        .iter()
        .map(|m| m.name.0.as_str().to_string())
        .chain(app.library_classes.iter().map(|lc| lc.name.0.as_str().to_string()))
        .chain(app.controllers.iter().map(|c| c.name.0.as_str().to_string()))
        .chain(app.concern_model_items.keys().map(|k| k.0.as_str().to_string()))
        .collect()
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
    /// A relative constant, as written.
    Const(String),
    /// `::A::B`.
    Absolute(String),
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
/// Whether a constant path starts at `::`.
fn is_absolute(node: &Node<'_>) -> bool {
    match node.as_constant_path_node() {
        Some(path) => path.parent().is_none_or(|parent| is_absolute(&parent)),
        None => false,
    }
}

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
            Some(p) if is_absolute(b) => Base::Absolute(p.join("::")),
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
        let constants = literal_constants(program.statements().body().iter().collect());
        let mut kept: Vec<Expr> = Vec::new();
        let mut carried = 0usize;
        for stmt in program.statements().body().iter() {
            let loc = stmt.location();
            let (start, end) = (loc.start_offset(), loc.end_offset());
            // A module or class the file defines that the tree carries
            // (`app::keep_initializer_defined`): reading its constant
            // here autoloads its emitted file, so it is defined where
            // Rails runs the definition, before the file's later
            // statements (a prepend of it in an `on_load` block).
            if let Some(name) = super::initializers::defined_at(&file, start) {
                let span = crate::span::Span { file: super::sources::file_id(&file), start: start as u32, end: end as u32 };
                let path = name.split("::").map(Symbol::from).collect();
                // The span is the definition's, not a written read.
                let mut read = Expr::new(span, ExprNode::Const { path });
                read.decisions |= crate::expr::GENERATED_CONST_REF;
                kept.push(guard(read, &label(&rel, &text, &stmt)));
                carried += 1;
                continue;
            }
            // A reader applied it, or part of it, or named why not.
            if consumed.iter().any(|&(a, b)| (a <= start && end <= b) || (start <= a && b <= end))
                || refused.iter().any(|&(a, b)| start <= a && b <= end)
                // Read after this pass (`content_helper_attribute_additions`).
                || text[start..end].contains("ActionText::ContentHelper.allowed_")
            {
                continue;
            }
            // A Zeitwerk callback ingest moved into its constant's body
            // (`load_hook_includes::lift_autoload_includes`): its check.
            let lifted: Vec<crate::app::LiftedAutoloadCallback> =
                app.lifted_autoload_callbacks.iter().filter(|l| l.file == file && l.start as usize == start).cloned().collect();
            if !lifted.is_empty() {
                let site = label(&rel, &text, &stmt);
                let span = crate::span::Span {
                    file: super::sources::file_id(&file),
                    start: start as u32,
                    end: end as u32,
                };
                // One check per constant (a loop's statement lifts to
                // each model it names).
                let checks: Result<Vec<Expr>, String> =
                    lifted.iter().map(|l| lifted_registration(&l.cpath, &site, span)).collect();
                match checks {
                    Ok(exprs) => {
                        for expr in exprs {
                            kept.push(guard(expr, &site));
                        }
                        super::initializers::consume(&file, start, end);
                        carried += 1;
                    }
                    Err(reason) => super::initializers::refuse(&file, start, end, reason),
                }
                continue;
            }
            let mut scan = Scan::new(inputs, fired, &mut framework).with_constants(constants.clone());
            let verdict = scan.statement(&stmt);
            if !matches!(verdict, Verdict::Refused(_)) {
                for cpath in std::mem::take(&mut scan.callback_cpaths) {
                    if !app.autoload_callback_cpaths.contains(&cpath) {
                        app.autoload_callback_cpaths.push(cpath);
                    }
                }
            }
            match verdict {
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
                Verdict::Deferred { refused: mut inner_refused, kept: mut inner_kept } => {
                    // A module the hook's block defines, carried as the
                    // top-level module it is (`hoist_hook_modules`), is
                    // read where the block defines it.
                    let hoisted = hoist_hook_modules(app, &stmt, &file);
                    if !hoisted.is_empty() {
                        inner_refused.retain(|(s, e, _)| {
                            let defined = hoisted.iter().any(|(start, _)| start == s);
                            if defined {
                                inner_kept.push((*s, *e));
                            }
                            !defined
                        });
                        inner_kept.sort_unstable();
                    }
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
                            app.library_classes.extend(hoisted.into_iter().map(|(_, lc)| lc));
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

/// `ActiveSupport.on_load(:hook) do module X … end … end` (Core's
/// essentials/config/initializers/shopify_session.rb:42, whose
/// `:active_support_test_case` block defines
/// `ActionControllerLikelySessionStore` and extends
/// `ActionController::Metal` with it): `module` in a block defines its
/// constant at the block's lexical scope, the file's top level, and
/// lazy_load_hooks' `class_eval` of the block does not move it. Each such
/// module is carried as the library class it would be at the top level,
/// as `on_load_reopen` carries one a lib file's hook defines, and the
/// block reads its constant where it defines it
/// (`initializers::define`, the DEFINITIONS rule above), so the hook's
/// later statements find it. Only a module with a one-segment literal
/// name the tree has no class or module of; a class in a block (a model
/// reopen, Core's model_extensions.rb) or a module the tree or the
/// runtime already has (a reopen) stays refused by name. Each carried
/// statement's start, with its module, for the caller to keep only when
/// it carries the hook.
fn hoist_hook_modules(app: &App, stmt: &Node<'_>, file: &str) -> Vec<(usize, LibraryClass)> {
    let mut hoisted = Vec::new();
    let Some(call) = stmt.as_call_node() else { return hoisted };
    if constant_id_str(&call.name()) != "on_load"
        || call.receiver().and_then(|r| super::util::constant_path_of(&r)).map(|p| p.join("::")).as_deref()
            != Some("ActiveSupport")
    {
        return hoisted;
    }
    let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) else { return hoisted };
    for inner in flatten_statements(body) {
        let Some(module) = inner.as_module_node() else { continue };
        let Some(path) = module_name_path(&module) else { continue };
        let [name] = path.as_slice() else { continue };
        let taken = app.library_classes.iter().any(|c| c.name.0.as_str() == name)
            || app.models.iter().any(|m| m.name.0.as_str() == name)
            || app.controllers.iter().any(|c| c.name.0.as_str() == name)
            || crate::analyze::runtime_reads::is_runtime_declared_namespace(name)
            || hoisted.iter().any(|(_, c): &(usize, LibraryClass)| c.name.0.as_str() == name);
        if taken {
            continue;
        }
        if let Ok(lc) = super::library_class::library_class_from_module_node_with_scope(&module, &[], file) {
            let start = inner.location().start_offset();
            super::initializers::define(file, start, name);
            hoisted.push((start, lc));
        }
    }
    hoisted
}

/// `Rails.autoloaders.main.on_load_lifted("Cpath", "site")`: the check a
/// lifted callback leaves at its registration.
fn lifted_registration(cpath: &str, site: &str, span: crate::span::Span) -> Result<Expr, String> {
    let quote = |text: &str| -> String { text.chars().filter(|c| !matches!(c, '"' | '\\' | '#')).collect() };
    let template = format!(
        "def __lifted\n  Rails.autoloaders.main.on_load_lifted(\"{}\", \"{}\")\nend\n",
        quote(cpath),
        quote(site)
    );
    let mut methods = crate::runtime_src::parse_methods(&template).map_err(|e| format!("does not ingest: {e}"))?;
    let method = methods.pop().ok_or("does not ingest")?;
    let mut expr = match &*method.body.node {
        ExprNode::Seq { exprs } if exprs.len() == 1 => exprs[0].clone(),
        _ => method.body,
    };
    expr.span = span;
    Ok(expr)
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
         $stderr.puts \"roundhouse: error[initializer_statement_not_run]: {site} did not run: #{{__initializer_error.class}}: #{{__initializer_error.message}}\"\n  \
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
        // A `#: self as T` comment is no statement: Ruby never runs it.
        // It stays a statement of the body itself, where the typer reads
        // it for the statements after it and the emitter drops it.
        if crate::expr::is_self_binding(&s) {
            kept.push(s);
            continue;
        }
        let label = labels.get(&s.span.start).cloned().unwrap_or_else(|| "initializer statement".into());
        kept.push(guard(s, &label));
    }
    let span = body.span;
    *body = Expr::new(span, ExprNode::Seq { exprs: kept });
    Ok(())
}

/// A carried `require` of `target`, a support file of declarations only
/// (`loaded_support`). Under the app's `lib/` its constants autoload as
/// Zeitwerk loads them: a no-op. Under a gem's `lib/` nothing else loads
/// them, as the emitted tree gives a plain gem's constants no autoload
/// (`project::apply_models_autoload`):
/// `RoundhouseAutoload.require_source("<path from the root>")`, which
/// loads the emitted files of the classes and modules it declares, here,
/// in its order, as Ruby requiring it does. Ascribed `untyped`, as the
/// generated loader is runtime the analyzer has no model of.
pub(super) fn require_of_support<V: Vfs + ?Sized>(inputs: &Inputs<'_, V>, target: &Path) -> ExprNode {
    if target.starts_with(inputs.dir.join("lib")) && !inputs.load_path.iter().skip(1).any(|lib| target.starts_with(lib)) {
        return ExprNode::Lit { value: Literal::Nil };
    }
    let span = crate::span::Span::synthetic();
    let recv = Expr::new(span, ExprNode::Const { path: vec![Symbol::from("RoundhouseAutoload")] });
    let source = Expr::new(span, ExprNode::Lit { value: Literal::Str { value: relative(inputs.dir, target) } });
    let call = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from("require_source"),
            args: vec![source],
            block: None,
            parenthesized: true,
        },
    );
    ExprNode::Cast { value: call, target_ty: crate::ty::Ty::Untyped }
}

/// `require "x"` → a runtime port, a no-op for an app lib file boot
/// already loads, the files of a gem file's declarations
/// ([`require_of_support`]), or the standard library feature itself;
/// anything else refuses.
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
        if let Some(t) = target.as_ref().filter(|t| inputs.loaded_support.contains(*t)) {
            *e.node = require_of_support(inputs, t);
            return;
        }
        if target.is_none() && STDLIB.contains(&feature.as_str()) {
            return;
        }
        // A file of a gem the tree compiles from source, which runs more
        // than declarations (Core's background_queue.rb initializer:
        // `require "hedwig/process"` and `"hedwig/workers"`, whose
        // `Hedwig::Process` hedwig's autoloads do not name): the emitted
        // `require` loads its compiled program, as Ruby loads the gem's
        // file. Refused, `Hedwig::Process.prepend(…)` below it raised
        // NameError at boot.
        if target.as_ref().is_some_and(|t| inputs.gem_files.contains(&relative(inputs.dir, t))) {
            return;
        }
        // A file of a path gem's (or the app's) `lib/` the tree carries
        // as a file program, which runs more than declarations (Core's
        // checkouts/one protobuf.rb initializer: `require
        // "checkout_protobuf_definitions"`, whose file requires every
        // `*_pb.rb` beside it): the emitted program, as Ruby loads the
        // file. Refused, nothing loaded the `_pb.rb` files and
        // `CheckoutProtobufDefinitions::PurchaseOrders::CustomFields` was
        // undefined where a codec named it.
        if let Some(rel) = target.as_ref().map(|t| relative(inputs.dir, t)).filter(|rel| inputs.lib_programs.contains(rel)) {
            // The holder is emitted at app/models/<holder>.rb.
            *method = Symbol::from("require_relative");
            if let ExprNode::Lit { value: Literal::Str { value } } = &mut *args[0].node {
                *value = format!("roundhouse_files/{}", rel.strip_suffix(".rb").unwrap_or(&rel));
            }
            return;
        }
        // On the test load path only (a test hook's `require
        // "support/helpers/…"`): the emitted `require` loads the carried
        // file there, in the test environment, as Ruby resolves it
        // natively (`project::test_load_path_features`).
        if target.is_none()
            && inputs.test_load_path.iter().filter(|root| inputs.vfs.exists(&root.join(&file))).count() == 1
        {
            return;
        }
        // A file of an installed gem the tree loads natively, which the
        // gem's entry does not itself require: the emitted `require`
        // loads it from the gem home.
        if target.is_none()
            && inputs.gems.is_some_and(|(home, lock)| super::gem_source_constants::locked_gem_owns(inputs.vfs, home, lock, &feature))
        {
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

/// What an `on_load` is.
enum Hook {
    /// `ActiveSupport.on_load(:hook)` the emitted app fires: the block
    /// runs on the hook's base.
    Fired,
    /// One a runtime port lifts itself.
    Lifted,
    /// Zeitwerk's `Rails.autoloaders.main.on_load("Cpath")`
    /// (runtime/spinel/rails_autoloaders.rb): the block runs at `main`
    /// when the constant's file loads.
    Autoload,
}

/// The admission rule (module doc), statement by statement.
pub(super) struct Scan<'s, 'a, V: Vfs + ?Sized> {
    inputs: &'s Inputs<'a, V>,
    fired: &'s FiredHooks,
    framework: &'s mut BTreeMap<String, FrameworkLoadHook>,
    /// Inside an `on_load` block: `self` is the hook's base.
    in_hook: usize,
    /// Inside a lambda (`->` or `lambda { }`): `return` leaves the lambda.
    in_lambda: usize,
    reason: Option<String>,
    /// An environment file's statement, not an initializer's.
    env: Option<EnvironmentRules<'s>>,
    /// The file's literal-valued constants ([`literal_constants`]).
    constants: LiteralConstants,
    /// The block parameters of the enclosing loops over a literal list of
    /// strings (`[...].each do |name|`), with the strings they take.
    loop_vars: Vec<(String, Vec<String>)>,
    /// The constant paths a Zeitwerk `on_load(<loop variable>)` of the
    /// statement registers for ([`App::autoload_callback_cpaths`]).
    callback_cpaths: Vec<String>,
}

/// A constant a file assigns a literal list to: an array's strings, or a
/// hash's string keys.
#[derive(Clone, Default)]
pub(super) struct LiteralConstants {
    pub(super) arrays: BTreeMap<String, Vec<String>>,
    pub(super) hash_keys: BTreeMap<String, Vec<String>>,
}

impl<'s, 'a, V: Vfs + ?Sized> Scan<'s, 'a, V> {
    fn new(inputs: &'s Inputs<'a, V>, fired: &'s FiredHooks, framework: &'s mut BTreeMap<String, FrameworkLoadHook>) -> Self {
        Scan {
            inputs,
            fired,
            framework,
            in_hook: 0,
            in_lambda: 0,
            reason: None,
            env: None,
            constants: LiteralConstants::default(),
            loop_vars: Vec::new(),
            callback_cpaths: Vec::new(),
        }
    }

    /// The file's literal-valued constants, which a loop may walk.
    fn with_constants(mut self, constants: LiteralConstants) -> Self {
        self.constants = constants;
        self
    }

    /// The admission rule for an environment file's statement.
    pub(super) fn environment(
        inputs: &'s Inputs<'a, V>,
        fired: &'s FiredHooks,
        framework: &'s mut BTreeMap<String, FrameworkLoadHook>,
        rules: EnvironmentRules<'s>,
    ) -> Self {
        let mut scan = Scan::new(inputs, fired, framework);
        scan.env = Some(rules);
        scan
    }

    pub(super) fn statement(&mut self, stmt: &Node<'_>) -> Verdict {
        if let Some(call) = stmt.as_call_node() {
            let name = constant_id_str(&call.name());
            if let Some(block) = call.block().and_then(|b| b.as_block_node()) {
                // `GraphApi::Admin.configure_query_root do … end`: the
                // block is an `on_load` block of the type, run later with
                // `self` the type (`ConfigureType`).
                let configure = self.configure_call(&call);
                if DEFERRED.contains(&name) || configure {
                    // The call minus its block.
                    if let Some(r) = call.receiver() {
                        self.visit(&r);
                    }
                    if let Some(args) = call.arguments() {
                        for a in args.arguments().iter() {
                            self.visit(&a);
                        }
                    }
                    // A load hook's block runs on its base; a Zeitwerk
                    // callback's at `main`, like the rest of the file.
                    let mut hook = name == "on_load" || configure;
                    if name == "on_load" {
                        match self.hook(&call) {
                            Ok(Hook::Fired) => {}
                            Ok(Hook::Autoload) => hook = false,
                            Ok(Hook::Lifted) => return Verdict::Lifted,
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

    /// The strings a read of an enclosing loop's block parameter takes.
    fn loop_paths(&self, node: &Node<'_>) -> Option<Vec<String>> {
        let name = constant_id_str(&node.as_local_variable_read_node()?.name()).to_string();
        self.loop_vars.iter().rev().find(|(var, _)| *var == name).map(|(_, values)| values.clone())
    }

    /// The strings `call`'s block parameter takes when `call` is
    /// `<list>.each do |name| … end` / `<hash>.each_key do |name| … end`
    /// (or `.keys.each`) over a literal array of strings, or a constant
    /// of the file that is one, or a hash with string keys.
    fn loop_binding(&self, call: &ruby_prism::CallNode<'_>) -> Option<(String, Vec<String>)> {
        let method = constant_id_str(&call.name());
        if !matches!(method, "each" | "each_key") || call.arguments().is_some() {
            return None;
        }
        let block = call.block()?.as_block_node()?;
        let params = block.parameters()?.as_block_parameters_node()?.parameters()?;
        if params.optionals().iter().next().is_some() || params.rest().is_some() || params.keywords().iter().next().is_some() {
            return None;
        }
        let mut required = params.requireds().iter();
        let param = constant_id_str(&required.next()?.as_required_parameter_node()?.name()).to_string();
        if required.next().is_some() {
            return None;
        }
        let values = literal_strings(&call.receiver()?, method == "each_key", &self.constants)?;
        Some((param, values))
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

    /// Whether `call` is `<Module>.configure_<type>` of a module that
    /// extends `GraphQL::Shopify::ConfigureType`, given a literal block.
    fn configure_call(&self, call: &ruby_prism::CallNode<'_>) -> bool {
        if !constant_id_str(&call.name()).starts_with("configure_") || self.fired.configure_modules.is_empty() {
            return false;
        }
        call.receiver()
            .and_then(|r| constant_path_segments_strs(&r))
            .is_some_and(|p| self.fired.configure_modules.contains(p.join("::").trim_start_matches("::")))
    }

    /// Which `on_load` `call` is, or why it cannot run.
    fn hook(&mut self, call: &ruby_prism::CallNode<'_>) -> Result<Hook, String> {
        let receiver = call.receiver().ok_or("a receiverless `on_load`")?;
        let path = constant_path_segments_strs(&receiver).map(|p| p.join("::"));
        if path.as_deref() != Some("ActiveSupport") {
            let loc = receiver.location();
            let text = String::from_utf8_lossy(loc.as_slice());
            if super::load_hook_includes::rails_autoloader(&receiver).is_some() {
                // runtime/spinel/rails_autoloaders.rb: the emitted file
                // of a constant a literal path names reports its load.
                let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                return match args.as_slice() {
                    [] => Err("Zeitwerk `on_load` without a constant path runs for every constant's load; the \
                               emitted tree reports the load only of a constant a literal path names"
                        .into()),
                    [cpath] if cpath.as_string_node().is_some() => Ok(Hook::Autoload),
                    // `names.each do |name| … on_load(name) … end` over a
                    // literal list: each string is a literal path, which
                    // `project` lists for its file to report its load.
                    [cpath] if self.loop_paths(cpath).is_some() => {
                        let paths = self.loop_paths(cpath).unwrap_or_default();
                        self.callback_cpaths.extend(paths);
                        Ok(Hook::Autoload)
                    }
                    _ => Err("Zeitwerk `on_load` with a computed constant path; the emitted tree reports the load \
                              only of a constant a literal path (or a loop variable over a literal list of them) \
                              names"
                        .into()),
                };
            }
            if text.contains("autoloaders") {
                return Err(format!(
                    "Zeitwerk `on_load` on `{}`, not `Rails.autoloaders.main` or `.once`, the loaders the \
                     emitted tree ports",
                    cap(&text, 40)
                ));
            }
            return Err(format!("`on_load` on `{}` is not ActiveSupport's load hook", cap(&text, 40)));
        }
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let Some((name, _)) = args.first().and_then(hook_name) else {
            return Err("`on_load` with a computed hook name".into());
        };
        if self.inputs.lifted.contains(&name.as_str()) {
            return Ok(Hook::Lifted);
        }
        if let Some((hook, base, site)) = FRAMEWORK_LOAD_HOOKS.iter().find(|(h, _, _)| *h == name) {
            self.framework.entry(name.clone()).or_insert_with(|| FrameworkLoadHook {
                hook: hook.to_string(),
                base: base.to_string(),
                site: site.to_string(),
            });
            return Ok(Hook::Fired);
        }
        if self.fired.bases.contains_key(&name) || TEST_LOAD_HOOKS.iter().any(|(h, _, _)| *h == name) {
            return Ok(Hook::Fired);
        }
        Err(format!(
            "nothing fires `:{name}`: no `ActiveSupport.run_load_hooks(:{name}, …)` in the runtime or the app's code"
        ))
    }
}

/// The base `on_load(:hook)`'s block runs on (`class_eval`), for the
/// analyzer: a framework or test-case hook's, or the one the app's code
/// fires it with.
pub fn hook_base(app: &App, hook: &str) -> Option<String> {
    if let Some((_, base, _)) = FRAMEWORK_LOAD_HOOKS.iter().chain(TEST_LOAD_HOOKS).find(|(h, _, _)| *h == hook) {
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

/// The strings of a literal list: an array of string literals, or with
/// `keys` the string keys of a hash literal (what `each_key` and `keys`
/// walk), directly, through `.freeze`/`.keys`, or as a constant the file
/// assigns one to.
pub(super) fn literal_strings(node: &Node<'_>, keys: bool, constants: &LiteralConstants) -> Option<Vec<String>> {
    if let Some(call) = node.as_call_node() {
        let recv = call.receiver()?;
        if call.arguments().is_some() || call.block().is_some() {
            return None;
        }
        return match constant_id_str(&call.name()) {
            "freeze" => literal_strings(&recv, keys, constants),
            "keys" if !keys => literal_strings(&recv, true, constants),
            _ => None,
        };
    }
    if let Some(array) = node.as_array_node() {
        return (!keys).then(|| array_strings(&array)).flatten();
    }
    if let Some(hash) = node.as_hash_node() {
        return keys.then(|| hash_keys(&hash)).flatten();
    }
    let path = constant_path_segments_strs(node)?.join("::");
    let path = path.trim_start_matches("::");
    if keys { constants.hash_keys.get(path) } else { constants.arrays.get(path) }.cloned()
}

fn array_strings(array: &ruby_prism::ArrayNode<'_>) -> Option<Vec<String>> {
    array
        .elements()
        .iter()
        .map(|e| e.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()))
        .collect()
}

fn hash_keys(hash: &ruby_prism::HashNode<'_>) -> Option<Vec<String>> {
    hash.elements()
        .iter()
        .map(|e| {
            let key = e.as_assoc_node()?.key();
            key.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
        })
        .collect()
}

/// The constants a file's modules and classes (nested as written) and its
/// top level assign a literal array of strings or hash with string keys
/// to, by full path (`LedgerSharding::MODELS`), through `.freeze`.
pub(super) fn literal_constants(stmts: Vec<Node<'_>>) -> LiteralConstants {
    fn walk(stmts: Vec<Node<'_>>, nesting: &[String], out: &mut LiteralConstants) {
        for stmt in stmts {
            if let Some(module) = stmt.as_module_node() {
                if let Some(path) = module_name_path(&module) {
                    let mut inner = nesting.to_vec();
                    inner.extend(path);
                    walk(module.body().map(flatten_statements).unwrap_or_default(), &inner, out);
                }
            } else if let Some(class) = stmt.as_class_node() {
                if let Some(path) = class_name_path(&class) {
                    let mut inner = nesting.to_vec();
                    inner.extend(path);
                    walk(class.body().map(flatten_statements).unwrap_or_default(), &inner, out);
                }
            } else if let Some(write) = stmt.as_constant_write_node() {
                let full = nesting.iter().cloned().chain([constant_id_str(&write.name()).to_string()]).collect::<Vec<_>>().join("::");
                let none = LiteralConstants::default();
                if let Some(values) = literal_strings(&write.value(), false, &none) {
                    out.arrays.insert(full, values);
                } else if let Some(values) = literal_strings(&write.value(), true, &none) {
                    out.hash_keys.insert(full, values);
                }
            }
        }
    }
    let mut out = LiteralConstants::default();
    walk(stmts, &[], &mut out);
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
            let rebinds = match self.scan.hook(node) {
                Ok(Hook::Fired) => true,
                Ok(Hook::Autoload) => false,
                Ok(Hook::Lifted) => {
                    self.scan.refuse("an adapter load hook inside another statement");
                    return;
                }
                Err(reason) => {
                    self.scan.refuse(reason);
                    return;
                }
            };
            if let Some(r) = node.receiver() {
                self.visit(&r);
            }
            if let Some(args) = node.arguments() {
                for a in args.arguments().iter() {
                    self.visit(&a);
                }
            }
            if let Some(block) = node.block() {
                self.scan.in_hook += usize::from(rebinds);
                self.visit(&block);
                self.scan.in_hook -= usize::from(rebinds);
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
        let binding = self.scan.loop_binding(node);
        let bound = binding.is_some();
        if let Some(binding) = binding {
            self.scan.loop_vars.push(binding);
        }
        let lambda = name == "lambda" && node.receiver().is_none() && node.block().is_some_and(|b| b.as_block_node().is_some());
        self.scan.in_lambda += usize::from(lambda);
        ruby_prism::visit_call_node(self, node);
        self.scan.in_lambda -= usize::from(lambda);
        if bound {
            self.scan.loop_vars.pop();
        }
    }
    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        self.scan.in_lambda += 1;
        ruby_prism::visit_lambda_node(self, node);
        self.scan.in_lambda -= 1;
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
    // A lambda's `return` (Core's `config.on_flag_error = ->(error, handle)
    // { return false if ... }` in essentials' initializers/verdict.rb)
    // returns from the lambda; elsewhere it ends the initializer's file.
    fn visit_return_node(&mut self, node: &ruby_prism::ReturnNode<'pr>) {
        if self.scan.in_lambda == 0 {
            self.scan.refuse("`return` in an initializer");
            return;
        }
        ruby_prism::visit_return_node(self, node);
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
