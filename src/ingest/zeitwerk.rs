//! A Zeitwerk loader a support file creates, compiled instead of run.
//!
//! Core's in-repo gems set up their own loader at require time
//! (`gems/functions/lib/functions.rb`):
//!
//! ```ruby
//! module Functions
//!   ZeitwerkLoader = Zeitwerk::Loader.for_gem #: Zeitwerk::Loader
//!   private_constant(:ZeitwerkLoader)
//!   ZeitwerkLoader.ignore(File.join(__dir__, "tapioca"))
//!   ZeitwerkLoader.setup
//! ```
//!
//! and a gem carried as source does the same at its file level
//! (graphql-shopify 0.20.3's `lib/graphql/shopify.rb`):
//!
//! ```ruby
//! LOADER = Zeitwerk::Loader.for_gem_extension(GraphQL) #: Zeitwerk::Loader
//! LOADER.inflector.inflect("html" => "HTML", "json" => "JSON", "url" => "URL")
//! LOADER.ignore("#{__dir__}/shopify/batch")
//! LOADER.setup
//! LOADER.eager_load_dir("#{__dir__}/shopify/patches")
//! ```
//!
//! (shopify_errors and schema-migrations hold theirs in a local,
//! `loader = Zeitwerk::Loader.for_gem`), and a gem's namespace module
//! can build one by hand (dux-event-reporter 0.4.1's
//! `lib/dux/event_reporter.rb`):
//!
//! ```ruby
//! module Dux
//!   module EventReporter
//!     LOADER = Zeitwerk::Loader.new
//!     private_constant :LOADER
//!     LOADER.tag = "dux-event-reporter"
//!     LOADER.push_dir(File.expand_path("event_reporter", __dir__), namespace: self)
//!     LOADER.ignore(File.expand_path("event_reporter/version.rb", __dir__))
//!     LOADER.setup
//! ```
//!
//! The emitted tree does not run Zeitwerk. The gem's `lib/` is already a
//! support root, so its files are carried and the generated
//! `app/models.rb` registers each one's constant with `Module#autoload`,
//! Zeitwerk's own model (`project::apply_models_autoload`). What the
//! loader adds is read here, statically: its root directory (the calling
//! file's directory for `for_gem` and `for_gem_extension`, the `push_dir`
//! argument for `Loader.new`), its root namespace (`Object`, the module
//! `for_gem_extension` names, or `push_dir`'s `namespace:`: `self` in a
//! class body, or a constant), the paths it ignores, its literal
//! inflection overrides, whether it is set up, and the directories it
//! eager-loads. A file under an ignored path, or under a loader that is
//! never set up, is not autoloaded or eager-loaded, exactly as Zeitwerk
//! leaves it alone. `tag=` of a literal only names the loader in
//! Zeitwerk's logs, which the compiled loader does not write. The loader
//! itself becomes the generated `RoundhouseAutoload.loader` handle, whose
//! `eager_load` loads the loader's files.
//!
//! A handle is only ever emitted for a loader the generated `LOADERS`
//! registers, so every declared loader has its root when it is declared.
//! `Zeitwerk::Loader.new` has none of its own: its declaration reads the
//! first `push_dir` on it among the later statements of the same body. A
//! `Loader.new` with no such `push_dir`, or with one this does not model,
//! is refused at the constructor; the calls on a refused loader add
//! nothing, since its own refusal raises before any of them runs.
//!
//! `eager_load_dir(dir)` stays at its site as a call on the handle with
//! the directory's namespace, which this compiles with the loader's
//! inflection (`patches` under `GraphQL` with root `lib/graphql/` and
//! `shopify/patches` is `GraphQL::Shopify::Patches`). As Zeitwerk does,
//! the handle loads the directory's files only when every segment of
//! that namespace is defined (an autoload counts) and is a no-op
//! otherwise; an ignored directory is a no-op. Native graphql-shopify
//! reaches the no-op: the call runs before the file defines
//! `GraphQL::Shopify` (Core's `config/initializers/graphql_shopify_patches.rb`
//! explains why, and requires the patches itself).
//!
//! The constant ingest sees the generated autoloads by content, not by
//! file name, so an inflection override changes nothing there; it is
//! read for the namespace an `eager_load_dir` compiles to.
//!
//! `collapse(pattern, …)` (database_documentation 3.2.0's
//! `loader.collapse("#{__dir__}/**/concerns")`) makes each directory the
//! patterns name add no namespace: `model_metadata/concerns/metadata.rb`
//! is `DatabaseDocumentation::ModelMetadata::Metadata`. Zeitwerk expands
//! the patterns with `Dir.glob` when `collapse` runs (zeitwerk 2.8.2
//! loader/config.rb:171); so does the compile, against the carried tree.
//! The loader records the directories, which the generated autoload
//! reads when it maps a file's path to its constant
//! (`project::apply_models_autoload`), and an `eager_load_dir` namespace
//! skips them, as Zeitwerk's walk does.
//!
//! Paths are evaluated at compile time from literals:
//! `File.join(__dir__, "x", …)`, `File.expand_path("x", __dir__)` and
//! `"#{__dir__}/x"` are the source file's directory joined with the
//! literal parts. Any other loader call (an inflector other than a
//! literal `inflect` hash, `eager_load` at load, reloading, callbacks,
//! a computed path) is a named refusal at its site: one in a class body
//! raises there when the body runs, one at file level is a file
//! statement the program names and does not run.
//!
//! Recorded through a thread-local collector like [`super::survey`],
//! since a class-body walk returns classes, not app facts;
//! `ingest::app` drains it after the support walk and resolves the
//! carried files against each loader.

use crate::ingest::prism_api::PrismLocation;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use ruby_prism::Node;

use crate::Symbol;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::ClassId;
use crate::span::Span;

use super::util::{constant_id_str, constant_path_of, string_value};

/// One loader a class body or a file's top level declared.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LoaderDecl {
    /// The handle's name: the constant holding it
    /// (`Functions::ZeitwerkLoader`, a file-level `LOADER`), or
    /// `local@app/relative/file.rb` for a file-level local.
    pub constant: String,
    /// The directory it manages, absolute like the walk's entries.
    pub root: PathBuf,
    /// The root directory's namespace (`["GraphQL"]` for
    /// `for_gem_extension(GraphQL)`, `["Dux", "EventReporter"]` for
    /// `push_dir(dir, namespace: self)` in that module); empty for
    /// `Object`.
    pub namespace: Vec<String>,
    /// Paths (file or directory) it ignores, absolute.
    pub ignores: Vec<PathBuf>,
    /// Whether the body calls `setup`.
    pub setup: bool,
    /// `inflector.inflect` overrides, basename => constant name.
    pub inflections: Vec<(String, String)>,
    /// Directories `eager_load_dir` was called on, absolute.
    pub eager_dirs: Vec<PathBuf>,
    /// Directories `collapse` names (its glob patterns expanded), absolute
    /// and sorted: each adds no namespace.
    pub collapsed: Vec<PathBuf>,
}

/// A loader constructor the walk has seen, under its handle name.
#[derive(Clone, Debug, PartialEq)]
enum Declared {
    /// Compiled: the handle the constant or local holds is registered.
    Modeled(LoaderDecl),
    /// Refused at its constructor; no handle names it.
    Refused(String),
}

impl Declared {
    fn key(&self) -> &str {
        match self {
            Declared::Modeled(decl) => &decl.constant,
            Declared::Refused(key) => key,
        }
    }
}

thread_local! {
    static LOADERS: RefCell<Vec<Declared>> = const { RefCell::new(Vec::new()) };
}

/// Every loader declared since the last drain, deduplicated (a file can
/// be walked twice: as a model's nested classes and as library classes).
pub(super) fn drain() -> Vec<LoaderDecl> {
    let all = LOADERS.with(|l| std::mem::take(&mut *l.borrow_mut()));
    let mut out: Vec<LoaderDecl> = Vec::new();
    for declared in all {
        if let Declared::Modeled(decl) = declared {
            if !out.contains(&decl) {
                out.push(decl);
            }
        }
    }
    out
}

fn with_loader<R>(constant: &str, f: impl FnOnce(&mut LoaderDecl) -> R) -> Option<R> {
    LOADERS.with(|l| match l.borrow_mut().iter_mut().rev().find(|d| d.key() == constant) {
        Some(Declared::Modeled(decl)) => Some(f(decl)),
        _ => None,
    })
}

/// What the walk last declared under `constant`: `Some(true)` for a
/// refused constructor, `Some(false)` for a compiled loader.
fn declared(constant: &str) -> Option<bool> {
    LOADERS.with(|l| {
        l.borrow().iter().rev().find(|d| d.key() == constant).map(|d| matches!(d, Declared::Refused(_)))
    })
}

/// Record a new loader. A second walk of the same file declares the
/// same one again: it restarts that declaration, so the calls the walk
/// applies next land on one record, not on a twin that is never set up.
fn declare(decl: LoaderDecl) {
    LOADERS.with(|l| {
        let mut loaders = l.borrow_mut();
        loaders.retain(|d| match d {
            Declared::Modeled(d) => {
                !(d.constant == decl.constant && d.root == decl.root && d.namespace == decl.namespace)
            }
            Declared::Refused(key) => *key != decl.constant,
        });
        loaders.push(Declared::Modeled(decl));
    });
}

/// Record a constructor refused under `key`, so the calls on it that
/// follow are known to be calls on a loader that never exists.
fn declare_refused(key: &str) {
    LOADERS.with(|l| {
        let mut loaders = l.borrow_mut();
        loaders.retain(|d| !matches!(d, Declared::Refused(k) if k == key));
        loaders.push(Declared::Refused(key.to_string()));
    });
}

/// Where a loader is written: a class body (`self` is its owner) or a
/// file's top level (`self` is main, and a local can hold the loader).
#[derive(Clone, Copy)]
enum Scope<'a> {
    Body(&'a ClassId),
    File,
}

/// The handle name a receiver refers to in `scope`: a constant (the
/// owner's, in a class body), or a file-level local.
fn key_of(receiver: &Node<'_>, scope: Scope<'_>, file: &str) -> Option<String> {
    if let Some(name) = receiver.as_constant_read_node() {
        let name = constant_id_str(&name.name());
        return Some(match scope {
            Scope::Body(owner) => qualified(owner, name),
            Scope::File => name.to_string(),
        });
    }
    match scope {
        Scope::File => receiver.as_local_variable_read_node().map(|local| local_key(constant_id_str(&local.name()), file)),
        Scope::Body(_) => None,
    }
}

fn qualified(owner: &ClassId, name: &str) -> String {
    if owner.0.as_str().is_empty() { name.to_string() } else { format!("{}::{name}", owner.0.as_str()) }
}

/// The handle name of a file-level local holding a loader.
fn local_key(name: &str, file: &str) -> String {
    format!("{name}@{}", super::sources::relative_path(file))
}

fn span_of(node: &Node<'_>, file: &str) -> Span {
    let loc = node.location();
    Span { file: super::sources::file_id(file), start: loc.start_offset() as u32, end: loc.end_offset() as u32 }
}

/// A `nil` the Ruby emitter renders as a `NotImplementedError` raise,
/// reported as `ingest_unsupported` (see `ingest::expr::refused_statement`).
fn refusal(node: &Node<'_>, file: &str, detail: String) -> Expr {
    let mut expr = Expr::new(span_of(node, file), ExprNode::Lit { value: Literal::Nil });
    expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::IngestRefused { detail });
    expr
}

fn str_lit(value: &str) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: value.to_string() } })
}

/// `RoundhouseAutoload.loader("Owner::Name")`, the generated handle.
///
/// Ascribed `untyped`: the handle class is generated runtime the
/// analyzer has no model of, and a constant whose value type is unknown
/// is refused at every read (`Functions::ZeitwerkLoader` inside the
/// gem). Gradual is the honest type; the Ruby emitter renders the cast
/// as the bare call. The cast keeps the written value's span: the
/// constant's declaration is found by it.
fn handle(constant: &str, span: Span) -> Expr {
    let recv = Expr::new(Span::synthetic(), ExprNode::Const { path: vec![Symbol::from("RoundhouseAutoload")] });
    let call = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from("loader"),
            args: vec![str_lit(constant)],
            block: None,
            parenthesized: true,
        },
    );
    Expr::new(span, ExprNode::Cast { value: call, target_ty: crate::ty::Ty::Untyped })
}

fn is_zeitwerk_loader(node: Option<Node<'_>>) -> bool {
    node.and_then(|r| constant_path_of(&r)).is_some_and(|path| {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        path == ["Zeitwerk", "Loader"]
    })
}

/// Declare the loader `value` creates under `key`; `write` is the
/// statement assigning it. `None` when `value` is not a
/// `Zeitwerk::Loader` constructor; otherwise the value the constant or
/// local holds: the generated handle of a loader declared with its root,
/// or a refusal for a form that is not modeled.
fn declare_loader(value: &Node<'_>, key: &str, file: &str, write: &Node<'_>, scope: Scope<'_>) -> Option<Expr> {
    let call = value.as_call_node()?;
    if !is_zeitwerk_loader(call.receiver()) {
        return None;
    }
    let refused = |detail: String| {
        declare_refused(key);
        Some(refusal(value, file, detail))
    };
    let method = constant_id_str(&call.name());
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    if call.block().is_some() {
        return refused(format!("`Zeitwerk::Loader.{method}` with a block is not modeled"));
    }
    let Some(calling_dir) = Path::new(file).parent().map(Path::to_path_buf) else {
        return refused(format!("`Zeitwerk::Loader.{method}` in a file with no directory is not modeled"));
    };
    let (root, namespace) = match method {
        // `for_gem` manages the calling file's directory.
        "for_gem" => {
            // `warn_on_extra_files:` only decides a warning at setup.
            for arg in &args {
                let only_warn_option = arg.as_keyword_hash_node().is_some_and(|hash| {
                    hash.elements().iter().all(|element| {
                        element.as_assoc_node().is_some_and(|assoc| {
                            super::util::symbol_value(&assoc.key()).as_deref() == Some("warn_on_extra_files")
                                && super::util::bool_value(&assoc.value()).is_some()
                        })
                    })
                });
                if !only_warn_option {
                    return refused(
                        "`Zeitwerk::Loader.for_gem` argument other than a literal `warn_on_extra_files:` is not modeled"
                            .into(),
                    );
                }
            }
            (calling_dir, Vec::new())
        }
        // `for_gem_extension(Namespace)`: the calling file's directory,
        // as that namespace (zeitwerk 2.8.2 loader.rb:436, `push_dir(root_dir,
        // namespace:)` in gem_loader.rb).
        "for_gem_extension" => {
            let namespace = match args.as_slice() {
                [one] if one.as_constant_read_node().is_some() || one.as_constant_path_node().is_some() => {
                    constant_path_of(one)
                }
                _ => None,
            };
            let Some(namespace) = namespace else {
                return refused(
                    "`Zeitwerk::Loader.for_gem_extension` of anything but a constant naming the namespace is not modeled"
                        .into(),
                );
            };
            let namespace: Vec<String> = namespace.into_iter().filter(|s| !s.is_empty()).collect();
            (calling_dir, namespace)
        }
        // `Loader.new` has no root until `push_dir` adds one: the first
        // `push_dir` on it later in the same body.
        "new" if args.is_empty() => match following_push_dir(write, key, file, scope) {
            Following::Root(root) => root,
            Following::Refused(detail) => {
                return refused(format!("`Zeitwerk::Loader.new` whose `push_dir` is not modeled: {detail}"));
            }
            Following::Absent => {
                return refused(
                    "`Zeitwerk::Loader.new` with no `push_dir` on it later in the same body is not modeled \
                     (the compiled loader is read from the root directory `push_dir` gives it)"
                        .into(),
                );
            }
            Following::Unreadable => {
                return refused(
                    "`Zeitwerk::Loader.new` in a file whose source text is not registered is not modeled \
                     (its later `push_dir` cannot be read)"
                        .into(),
                );
            }
        },
        other => {
            return refused(format!("`Zeitwerk::Loader.{other}` is not modeled"));
        }
    };
    declare(LoaderDecl {
        constant: key.to_string(),
        root,
        namespace,
        ignores: Vec::new(),
        setup: false,
        inflections: Vec::new(),
        eager_dirs: Vec::new(),
        collapsed: Vec::new(),
    });
    Some(handle(key, span_of(value, file)))
}

/// What [`following_push_dir`] finds for a `Loader.new`.
enum Following {
    /// The root directory and namespace the first later `push_dir` adds.
    Root((PathBuf, Vec<String>)),
    /// That `push_dir` is not modeled, and why.
    Refused(String),
    /// No later statement of the body calls `push_dir` on the loader, or
    /// the write is not a statement of a body.
    Absent,
    /// The file's text is not registered, so the body cannot be read again.
    Unreadable,
}

/// The first statement after `write` in the body holding it that calls
/// `push_dir` on the loader `key`.
///
/// The walk hands this module one statement at a time, so the body is
/// read again from the file's registered text.
fn following_push_dir(write: &Node<'_>, key: &str, file: &str, scope: Scope<'_>) -> Following {
    struct Body<'a, 's> {
        write: (usize, usize),
        key: &'a str,
        file: &'a str,
        scope: Scope<'s>,
        found: Option<Option<Result<(PathBuf, Vec<String>), String>>>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Body<'_, '_> {
        fn visit_statements_node(&mut self, node: &ruby_prism::StatementsNode<'pr>) {
            if self.found.is_some() {
                return;
            }
            let body: Vec<Node<'pr>> = node.body().iter().collect();
            let at = body.iter().position(|stmt| {
                (stmt.location().start_offset(), stmt.location().end_offset()) == self.write
            });
            let Some(at) = at else {
                ruby_prism::visit_statements_node(self, node);
                return;
            };
            self.found = Some(body[at + 1..].iter().find_map(|stmt| {
                let call = stmt.as_call_node()?;
                if constant_id_str(&call.name()) != "push_dir" {
                    return None;
                }
                let on_loader = call.receiver().and_then(|r| key_of(&r, self.scope, self.file));
                (on_loader.as_deref() == Some(self.key)).then(|| push_dir_root(&call, self.file, self.scope))
            }));
        }
    }
    let loc = write.location();
    let mut body =
        Body { write: (loc.start_offset(), loc.end_offset()), key, file, scope, found: None };
    let read = super::sources::with_text(file, |text| {
        let parsed = ruby_prism::parse(text.as_bytes());
        ruby_prism::Visit::visit(&mut body, &parsed.node());
    });
    match (read, body.found.flatten()) {
        (None, _) => Following::Unreadable,
        (Some(()), Some(Ok(root))) => Following::Root(root),
        (Some(()), Some(Err(detail))) => Following::Refused(detail),
        (Some(()), None) => Following::Absent,
    }
}

/// `push_dir(path)` or `push_dir(path, namespace: ns)` (zeitwerk 2.8.2
/// loader/config.rb:129): the absolute root it adds and the root's
/// namespace, or why the call is not modeled.
fn push_dir_root(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    scope: Scope<'_>,
) -> Result<(PathBuf, Vec<String>), String> {
    if call.block().is_some() {
        return Err("`Zeitwerk::Loader#push_dir` with a block is not modeled".into());
    }
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let (path, options) = match args.as_slice() {
        [path] => (path, None),
        [path, options] if options.as_keyword_hash_node().is_some() => (path, options.as_keyword_hash_node()),
        _ => {
            return Err(
                "`Zeitwerk::Loader#push_dir` with arguments other than a path and `namespace:` is not modeled".into()
            );
        }
    };
    let Some(path) = literal_path(path, file) else {
        return Err("`Zeitwerk::Loader#push_dir` of a path that is not a literal under `__dir__` is not modeled".into());
    };
    let mut namespace = Vec::new();
    for element in options.iter().flat_map(|hash| hash.elements().iter()) {
        let keyword = element.as_assoc_node().and_then(|assoc| {
            (super::util::symbol_value(&assoc.key()).as_deref() == Some("namespace")).then(|| assoc.value())
        });
        let Some(value) = keyword else {
            return Err(
                "`Zeitwerk::Loader#push_dir` with a keyword other than `namespace:` raises ArgumentError, which is \
                 not modeled"
                    .into(),
            );
        };
        namespace = push_dir_namespace(&value, scope)?;
    }
    Ok((path, namespace))
}

/// `push_dir`'s `namespace:` as a constant path, empty for `Object`.
/// `self` is the class body's module; at file level it is main, which
/// Zeitwerk rejects. A constant is read as written at file level and
/// when rooted (`::Name`); a relative one in a class body resolves
/// lexically, which is not modeled.
fn push_dir_namespace(value: &Node<'_>, scope: Scope<'_>) -> Result<Vec<String>, String> {
    if value.as_self_node().is_some() {
        return match scope {
            Scope::Body(owner) => Ok(owner.0.as_str().split("::").map(str::to_string).collect()),
            Scope::File => Err("`Zeitwerk::Loader#push_dir` with `namespace: self` at file level raises \
                                Zeitwerk::Error (main is not a class or module object), which is not modeled"
                .into()),
        };
    }
    fn static_path(node: &Node<'_>) -> bool {
        node.as_constant_read_node().is_some()
            || node.as_constant_path_node().is_some_and(|p| p.parent().is_none_or(|parent| static_path(&parent)))
    }
    if static_path(value) {
        let rooted = value.as_constant_path_node().is_some_and(|p| super::util::constant_path_is_rooted(&p));
        if rooted || matches!(scope, Scope::File) {
            let path = constant_path_of(value).unwrap_or_default();
            return Ok(if path == ["Object"] { Vec::new() } else { path });
        }
        return Err("`Zeitwerk::Loader#push_dir` with a relative constant `namespace:` in a class body (resolved \
                    lexically) is not modeled; `self` or a rooted `::Name` is"
            .into());
    }
    Err("`Zeitwerk::Loader#push_dir` with a `namespace:` other than `self` or a constant is not modeled".into())
}

/// `NAME = Zeitwerk::Loader.for_gem` / `.for_gem_extension(Ns)` /
/// `Zeitwerk::Loader.new`, in a class body (`owner`) or at file level
/// (`owner` empty: the constant is Object's). `None` when the value is
/// not a loader. Otherwise the value the constant holds: the generated
/// handle, or a refusal for a form that is not modeled.
pub(super) fn loader_constant(
    write: &ruby_prism::ConstantWriteNode<'_>,
    owner: &ClassId,
    file: &str,
) -> Option<Expr> {
    let constant = qualified(owner, constant_id_str(&write.name()));
    let scope = if owner.0.as_str().is_empty() { Scope::File } else { Scope::Body(owner) };
    declare_loader(&write.value(), &constant, file, &write.as_node(), scope)
}

/// What a call on a loader means.
enum Applied {
    /// Applied statically: nothing runs at the site.
    Static,
    /// Runs at the site (`eager_load_dir` on the handle).
    Runs(Expr),
    /// Not modeled: named at the site.
    Refused(String),
}

/// A class-body call on a loader this body declared (`NAME.setup`,
/// `NAME.ignore(…)`, `NAME.push_dir(…)`). `None` when `stmt` is not
/// one. `Some(None)` when it is applied statically; `Some(Some(refusal))`
/// for a loader call that is not modeled, to keep at its site.
pub(super) fn loader_call(stmt: &Node<'_>, owner: &ClassId, file: &str) -> Option<Option<Expr>> {
    match apply(stmt, file, Scope::Body(owner))? {
        Applied::Static => Some(None),
        Applied::Runs(_) => unreachable!("a class-body loader call never runs at its site"),
        Applied::Refused(detail) => Some(Some(refusal(stmt, file, detail))),
    }
}

/// A file-level statement on a loader: a local the file assigns one to
/// (`loader = Zeitwerk::Loader.for_gem`), or a call on a loader the file
/// holds in a local or a file-level constant. `None` when `stmt` is
/// neither; otherwise the program item it becomes, if any.
pub(super) fn file_loader_statement(stmt: &Node<'_>, file: &str) -> Option<Option<crate::app::ProgramItem>> {
    use crate::app::ProgramItem;
    if let Some(write) = stmt.as_local_variable_write_node() {
        let key = local_key(constant_id_str(&write.name()), file);
        let value = declare_loader(&write.value(), &key, file, stmt, Scope::File)?;
        let mut assign = super::expr::ingest_expr(stmt, file).ok()?;
        if let ExprNode::Assign { target: LValue::Var { .. }, value: written } = &mut *assign.node {
            *written = value;
        } else {
            return None;
        }
        return Some(Some(ProgramItem::Statement(assign)));
    }
    Some(match apply(stmt, file, Scope::File)? {
        Applied::Static => None,
        Applied::Runs(expr) => Some(ProgramItem::Statement(expr)),
        Applied::Refused(message) => Some(ProgramItem::NotCarried {
            span: span_of(stmt, file),
            construct: stmt
                .as_call_node()
                .map(|c| {
                    let on_inflector = c
                        .receiver()
                        .and_then(|r| r.as_call_node())
                        .is_some_and(|r| constant_id_str(&r.name()) == "inflector");
                    let owner = if on_inflector { "Zeitwerk::Inflector" } else { "Zeitwerk::Loader" };
                    format!("{owner}#{}", constant_id_str(&c.name()))
                })
                .unwrap_or_else(|| "Zeitwerk::Loader".into()),
            message,
        }),
    })
}

/// A call `stmt` on a loader: the innermost receiver of a chain
/// (`LOADER.inflector.inflect(…)` calls `inflector` on it), resolved to a
/// declared loader in `scope`. `None` when `stmt` is not one.
fn apply(stmt: &Node<'_>, file: &str, scope: Scope<'_>) -> Option<Applied> {
    let outer = stmt.as_call_node()?;
    let mut call = stmt.as_call_node()?;
    let key = loop {
        let receiver = call.receiver()?;
        if let Some(key) = key_of(&receiver, scope, file) {
            break key;
        }
        call = receiver.as_call_node()?;
    };
    // A call on a refused constructor's loader: the constructor's refusal
    // raises where the loader would be created, before this runs, and it
    // already names the loader.
    if declared(&key)? {
        return Some(Applied::Static);
    }
    let file_level = matches!(scope, Scope::File);
    let method = constant_id_str(&call.name());
    let chained = call.location().end_offset() != stmt.location().end_offset();
    if chained {
        // `inflector.inflect("html" => "HTML", …)` with literal strings.
        let direct = outer.receiver().is_some_and(|r| r.location().start_offset() == call.location().start_offset()
            && r.location().end_offset() == call.location().end_offset());
        if method == "inflector" && call.arguments().is_none() && direct && constant_id_str(&outer.name()) == "inflect" {
            if outer.block().is_some() {
                return Some(Applied::Refused("`Zeitwerk::Inflector#inflect` with a block is not modeled".into()));
            }
            let Some(overrides) = literal_inflections(&outer) else {
                return Some(Applied::Refused(
                    "`Zeitwerk::Inflector#inflect` of anything but one literal `\"basename\" => \"Constant\"` hash is not modeled"
                        .into(),
                ));
            };
            with_loader(&key, |d| {
                for (from, to) in overrides {
                    d.inflections.retain(|(f, _)| *f != from);
                    d.inflections.push((from, to));
                }
            });
            return Some(Applied::Static);
        }
        let detail = if method == "inflector" {
            format!("`Zeitwerk::Inflector#{}` is not modeled", constant_id_str(&outer.name()))
        } else {
            format!("`Zeitwerk::Loader#{method}` is not modeled")
        };
        return Some(Applied::Refused(detail));
    }
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    if call.block().is_some() {
        return Some(Applied::Refused(format!("`Zeitwerk::Loader#{method}` with a block is not modeled")));
    }
    match method {
        "setup" if args.is_empty() => {
            with_loader(&key, |d| d.setup = true);
            Some(Applied::Static)
        }
        "ignore" => {
            let mut paths = Vec::new();
            for arg in &args {
                match literal_path(arg, file) {
                    Some(path) => paths.push(path),
                    None => {
                        return Some(Applied::Refused(
                            "`Zeitwerk::Loader#ignore` of a path that is not `File.join(__dir__, …)` or another \
                             literal under `__dir__` is not modeled"
                                .into(),
                        ));
                    }
                }
            }
            with_loader(&key, |d| {
                for path in paths {
                    if !d.ignores.contains(&path) {
                        d.ignores.push(path);
                    }
                }
            });
            Some(Applied::Static)
        }
        // `collapse(pattern, …)` (zeitwerk 2.8.2 loader/config.rb:171):
        // each pattern is expanded with `Dir.glob` when the call runs, and
        // a directory it names adds no namespace.
        "collapse" => {
            let mut dirs = Vec::new();
            for arg in &args {
                let Some(pattern) = literal_path(arg, file) else {
                    return Some(Applied::Refused(
                        "`Zeitwerk::Loader#collapse` of a path that is not a literal under `__dir__` is not modeled"
                            .into(),
                    ));
                };
                match glob_dirs(&pattern) {
                    Some(found) => dirs.extend(found),
                    None => {
                        return Some(Applied::Refused(
                            "`Zeitwerk::Loader#collapse` of a glob with a character class, braces or an escape is not \
                             modeled (`*`, `?` and `**` are)"
                                .into(),
                        ));
                    }
                }
            }
            with_loader(&key, |d| {
                for dir in dirs {
                    if !d.collapsed.contains(&dir) {
                        d.collapsed.push(dir);
                    }
                }
                d.collapsed.sort();
            });
            Some(Applied::Static)
        }
        // The root a `Loader.new` was declared with (`following_push_dir`),
        // or a `for_gem` loader's own directory again; Zeitwerk maps the
        // directory to the namespace given last.
        "push_dir" => {
            let (path, namespace) = match push_dir_root(&call, file, scope) {
                Ok(root) => root,
                Err(detail) => return Some(Applied::Refused(detail)),
            };
            let same_root = with_loader(&key, |d| {
                let same = d.root == path;
                if same {
                    d.namespace = namespace;
                }
                same
            });
            if same_root == Some(false) {
                return Some(Applied::Refused("a Zeitwerk loader with more than one root directory is not modeled".into()));
            }
            Some(Applied::Static)
        }
        // Zeitwerk's `tag=` (loader/config.rb:160) only names the loader in
        // its log lines, which the compiled loader does not write.
        "tag=" if args.len() == 1 => {
            if string_value(&args[0]).is_some() || super::util::symbol_value(&args[0]).is_some() {
                Some(Applied::Static)
            } else {
                Some(Applied::Refused("`Zeitwerk::Loader#tag=` of anything but a literal string is not modeled".into()))
            }
        }
        "eager_load_dir" if args.len() == 1 && file_level => {
            let Some(path) = literal_path(&args[0], file) else {
                return Some(Applied::Refused(
                    "`Zeitwerk::Loader#eager_load_dir` of a path that is not a literal under `__dir__` is not modeled"
                        .into(),
                ));
            };
            let decl = with_loader(&key, |d| d.clone())?;
            Some(eager_load_dir(&decl, &key, &path, stmt, file))
        }
        other => Some(Applied::Refused(format!("`Zeitwerk::Loader#{other}` is not modeled"))),
    }
}

/// `"html" => "HTML", …`: every pair a literal string.
fn literal_inflections(call: &ruby_prism::CallNode<'_>) -> Option<Vec<(String, String)>> {
    let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
    let [arg] = args.as_slice() else { return None };
    let elements: Vec<Node<'_>> = if let Some(hash) = arg.as_keyword_hash_node() {
        hash.elements().iter().collect()
    } else {
        arg.as_hash_node()?.elements().iter().collect()
    };
    elements
        .iter()
        .map(|element| {
            let assoc = element.as_assoc_node()?;
            Some((string_value(&assoc.key())?, string_value(&assoc.value())?))
        })
        .collect()
}

/// The directories `Dir.glob(pattern)` names under Ruby's default flags,
/// sorted: `*` and `?` within a component, neither matching a leading
/// `.`, and `**` as a whole component for zero or more directories,
/// neither hidden nor reached through a symlink (`**` alone is `*`).
/// `None` for a character class, braces or an escape, which this does
/// not model.
fn glob_dirs(pattern: &Path) -> Option<Vec<PathBuf>> {
    let text = pattern.to_string_lossy();
    if text.contains(['[', ']', '{', '}', '\\']) {
        return None;
    }
    let mut base = PathBuf::new();
    let mut rest: Vec<String> = Vec::new();
    for component in pattern.components() {
        let part = component.as_os_str().to_string_lossy();
        if rest.is_empty() && !part.contains(['*', '?']) {
            base.push(component);
        } else {
            rest.push(part.into_owned());
        }
    }
    let mut out = Vec::new();
    glob_expand(&base, &rest, &mut out);
    out.sort();
    out.dedup();
    Some(out)
}

fn glob_expand(dir: &Path, rest: &[String], out: &mut Vec<PathBuf>) {
    let Some((head, tail)) = rest.split_first() else {
        if dir.is_dir() {
            out.push(dir.to_path_buf());
        }
        return;
    };
    let children = || -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
            .collect();
        found.sort();
        found
    };
    if head == "**" && !tail.is_empty() {
        glob_expand(dir, tail, out);
        for (name, path) in children() {
            let real_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
            if real_dir && !name.starts_with('.') {
                glob_expand(&path, rest, out);
            }
        }
        return;
    }
    if !head.contains(['*', '?']) {
        glob_expand(&dir.join(head), tail, out);
        return;
    }
    let head = if head == "**" { "*" } else { head.as_str() };
    for (name, path) in children() {
        if (!name.starts_with('.') || head.starts_with('.')) && wildcard_match(head.as_bytes(), name.as_bytes()) {
            glob_expand(&path, tail, out);
        }
    }
}

/// `*` (any run) and `?` (one character) against a whole name.
fn wildcard_match(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| wildcard_match(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && wildcard_match(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && wildcard_match(rest, &name[1..]),
    }
}

/// The constant segments of `relative` (components under the loader's
/// root, the last a file's stem when `file` is set), camelized: a
/// directory the loader collapses adds none, as Zeitwerk's walk skips it.
fn constant_segments(decl: &LoaderDecl, relative: &[String], file: bool) -> Vec<String> {
    let mut at = decl.root.clone();
    let mut out = Vec::new();
    for (i, segment) in relative.iter().enumerate() {
        at.push(segment);
        let is_dir = !(file && i + 1 == relative.len());
        if is_dir && decl.collapsed.contains(&at) {
            continue;
        }
        out.push(camelize(segment, &decl.inflections));
    }
    out
}

/// Zeitwerk's constant name for a file or directory basename
/// (`Zeitwerk::Inflector#camelize`): an override, else each `_` part
/// capitalized. A gem loader's version file is `VERSION`, which no
/// directory is.
fn camelize(basename: &str, inflections: &[(String, String)]) -> String {
    if let Some((_, to)) = inflections.iter().find(|(from, _)| from == basename) {
        return to.clone();
    }
    basename
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// `eager_load_dir(path)` (zeitwerk 2.8.2 loader/eager_load.rb:33):
/// before `setup` it raises; an ignored directory (or one under an
/// ignored path) returns; a directory outside the root raises "I do not
/// manage"; otherwise the directory's namespace, walked from the root
/// namespace, decides at run time whether its files load: it returns at
/// the first segment not defined, and at the calling file's own constant
/// while that is still only an autoload.
fn eager_load_dir(decl: &LoaderDecl, key: &str, path: &Path, stmt: &Node<'_>, file: &str) -> Applied {
    if !decl.setup {
        return Applied::Refused(
            "`Zeitwerk::Loader#eager_load_dir` before `setup` raises Zeitwerk::SetupRequired, which is not modeled"
                .into(),
        );
    }
    let root = &decl.root;
    if decl.ignores.iter().any(|ignore| path.starts_with(ignore)) {
        return Applied::Static;
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return Applied::Refused(format!(
            "`Zeitwerk::Loader#eager_load_dir` of {} raises (\"I do not manage\"), which is not modeled",
            super::sources::relative_path(&path.display().to_string())
        ));
    };
    let segments: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if segments.iter().any(|s| s.starts_with('.')) {
        // A hidden directory: Zeitwerk returns.
        return Applied::Static;
    }
    let namespace: Vec<String> =
        decl.namespace.iter().cloned().chain(constant_segments(decl, &segments, false)).collect();
    // The constant the calling file itself defines under this loader:
    // while Ruby requires the file, its autoload reads as undefined until
    // the file opens it, so an `eager_load_dir` under it (graphql-shopify's
    // `shopify/patches`, called before `module GraphQL::Shopify`) returns.
    let own: Option<Vec<String>> = Path::new(file).strip_prefix(root).ok().and_then(|rel| {
        let rel = rel.with_extension("");
        let rel: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        let own: Vec<String> = decl.namespace.iter().cloned().chain(constant_segments(decl, &rel, true)).collect();
        namespace.starts_with(&own).then_some(own)
    });
    with_loader(key, |d| {
        if !d.eager_dirs.iter().any(|dir| dir == path) {
            d.eager_dirs.push(path.to_path_buf());
        }
    });
    let call = Expr::new(
        span_of(stmt, file),
        ExprNode::Send {
            recv: Some(handle(key, Span::synthetic())),
            method: Symbol::from("eager_load_dir"),
            args: vec![
                str_lit(&namespace.join("::")),
                str_lit(&segments.join("/")),
                match own {
                    Some(own) => str_lit(&own.join("::")),
                    None => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
                },
            ],
            block: None,
            parenthesized: true,
        },
    );
    Applied::Runs(call)
}

/// The absolute path a literal path expression under `__dir__` names.
fn literal_path(node: &Node<'_>, file: &str) -> Option<PathBuf> {
    let dir = Path::new(file).parent()?;
    let is_dir = |n: &Node<'_>| {
        n.as_call_node().is_some_and(|c| {
            c.receiver().is_none() && c.arguments().is_none() && constant_id_str(&c.name()) == "__dir__"
        })
    };
    let join = |parts: Vec<String>| -> Option<PathBuf> {
        let mut path = dir.to_path_buf();
        for part in parts {
            for piece in part.split('/').filter(|p| !p.is_empty() && *p != ".") {
                if piece == ".." {
                    path.pop();
                } else {
                    path.push(piece);
                }
            }
        }
        Some(path)
    };
    if let Some(call) = node.as_call_node() {
        let on_file = call.receiver().and_then(|r| constant_path_of(&r)).is_some_and(|p| p == ["File"]);
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        if on_file && constant_id_str(&call.name()) == "join" && args.first().is_some_and(|a| is_dir(a)) {
            let parts: Option<Vec<String>> = args[1..].iter().map(string_value).collect();
            return join(parts?);
        }
        if on_file && constant_id_str(&call.name()) == "expand_path" && args.len() == 2 && is_dir(&args[1]) {
            return join(vec![string_value(&args[0])?]);
        }
        return None;
    }
    // `"#{__dir__}/tapioca"`
    let interpolated = node.as_interpolated_string_node()?;
    let parts: Vec<Node<'_>> = interpolated.parts().iter().collect();
    let first = parts.first()?.as_embedded_statements_node()?;
    let inner: Vec<Node<'_>> = first.statements()?.body().iter().collect();
    if inner.len() != 1 || !is_dir(&inner[0]) {
        return None;
    }
    let rest: Option<Vec<String>> = parts[1..]
        .iter()
        .map(|p| p.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()))
        .collect();
    join(rest?)
}

/// What a set of loaders means for the carried support files: per loader,
/// the classes it manages, and the classes no loader autoloads (under an
/// ignored path, or under a loader that is never set up). `files` is
/// every support file the walk ingested with the classes it produced.
///
/// A class a skipped file only reopens is not left alone:
/// dux-event-reporter ignores its `event_reporter/version.rb`, which
/// opens the loader's own namespace `Dux::EventReporter` to set
/// `VERSION`. Zeitwerk leaves that file's constant path unmapped, not
/// the namespace the gem's entry defines, so a class a file outside the
/// skipped set also defines stays registered.
pub(super) fn resolve(decls: &[LoaderDecl], files: &[(PathBuf, Vec<ClassId>)]) -> Vec<crate::app::ZeitwerkLoader> {
    decls
        .iter()
        .map(|decl| {
            let root = &decl.root;
            let skipped = |path: &Path| {
                path.starts_with(root) && (!decl.setup || decl.ignores.iter().any(|ignore| path.starts_with(ignore)))
            };
            let defined_elsewhere: std::collections::BTreeSet<&ClassId> =
                files.iter().filter(|(path, _)| !skipped(path)).flat_map(|(_, classes)| classes.iter()).collect();
            let mut managed = Vec::new();
            let mut ignored = Vec::new();
            let mut managed_files: Vec<(&PathBuf, &Vec<ClassId>)> = Vec::new();
            for (path, classes) in files {
                if !path.starts_with(root) {
                    continue;
                }
                if skipped(path) {
                    ignored.extend(classes.iter().filter(|c| !defined_elsewhere.contains(c)).cloned());
                } else {
                    managed.extend(classes.iter().cloned());
                    managed_files.push((path, classes));
                }
            }
            let eager_dirs = decl
                .eager_dirs
                .iter()
                .filter_map(|dir| {
                    let key = dir.strip_prefix(root).ok()?.display().to_string();
                    let mut sources: Vec<String> = Vec::new();
                    let mut classes: Vec<ClassId> = Vec::new();
                    for (path, owned) in managed_files.iter().filter(|(path, _)| path.starts_with(dir)) {
                        sources.push(path.display().to_string());
                        classes.extend(owned.iter().cloned());
                    }
                    sources.sort();
                    Some(crate::app::ZeitwerkEagerDir { dir: key, sources, classes })
                })
                .collect();
            crate::app::ZeitwerkLoader {
                constant: decl.constant.clone(),
                root: root.display().to_string(),
                managed,
                ignored,
                eager_dirs,
                collapsed: decl.collapsed.iter().map(|dir| dir.display().to_string()).collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::camelize;

    #[test]
    fn camelize_is_zeitwerks() {
        let overrides = vec![("html".to_string(), "HTML".to_string())];
        assert_eq!(camelize("patches", &overrides), "Patches");
        assert_eq!(camelize("html", &overrides), "HTML");
        assert_eq!(camelize("json", &overrides), "Json");
        assert_eq!(camelize("ast_analyzers", &overrides), "AstAnalyzers");
    }
}
