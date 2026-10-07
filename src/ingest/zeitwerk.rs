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
//! `loader = Zeitwerk::Loader.for_gem`).
//!
//! The emitted tree does not run Zeitwerk. The gem's `lib/` is already a
//! support root, so its files are carried and the generated
//! `app/models.rb` registers each one's constant with `Module#autoload`,
//! Zeitwerk's own model (`project::apply_models_autoload`). What the
//! loader adds is read here, statically: its root directory (the calling
//! file's directory for `for_gem` and `for_gem_extension`, the `push_dir`
//! argument for `Loader.new`), its root namespace (`Object`, or the
//! module `for_gem_extension` names), the paths it ignores, its literal
//! inflection overrides, whether it is set up, and the directories it
//! eager-loads. A file under an ignored path, or under a loader that is
//! never set up, is not autoloaded or eager-loaded, exactly as Zeitwerk
//! leaves it alone. The loader itself becomes the generated
//! `RoundhouseAutoload.loader` handle, whose `eager_load` loads the
//! loader's files.
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
//! Paths are evaluated at compile time from literals:
//! `File.join(__dir__, "x", …)`, `File.expand_path("x", __dir__)` and
//! `"#{__dir__}/x"` are the source file's directory joined with the
//! literal parts. Any other loader call (an inflector other than a
//! literal `inflect` hash, `collapse`, `eager_load` at load, reloading,
//! callbacks, a computed path) is a named refusal at its site: one in a
//! class body raises there when the body runs, one at file level is a
//! file statement the program names and does not run.
//!
//! Recorded through a thread-local collector like [`super::survey`],
//! since a class-body walk returns classes, not app facts;
//! `ingest::app` drains it after the support walk and resolves the
//! carried files against each loader.

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
    pub root: Option<PathBuf>,
    /// The root directory's namespace (`["GraphQL"]` for
    /// `for_gem_extension(GraphQL)`); empty for `Object`.
    pub namespace: Vec<String>,
    /// Paths (file or directory) it ignores, absolute.
    pub ignores: Vec<PathBuf>,
    /// Whether the body calls `setup`.
    pub setup: bool,
    /// `inflector.inflect` overrides, basename => constant name.
    pub inflections: Vec<(String, String)>,
    /// Directories `eager_load_dir` was called on, absolute.
    pub eager_dirs: Vec<PathBuf>,
}

thread_local! {
    static LOADERS: RefCell<Vec<LoaderDecl>> = const { RefCell::new(Vec::new()) };
}

/// Every loader declared since the last drain, deduplicated (a file can
/// be walked twice: as a model's nested classes and as library classes).
pub(super) fn drain() -> Vec<LoaderDecl> {
    let mut all = LOADERS.with(|l| std::mem::take(&mut *l.borrow_mut()));
    let mut out: Vec<LoaderDecl> = Vec::new();
    for decl in all.drain(..) {
        if !out.contains(&decl) {
            out.push(decl);
        }
    }
    out
}

fn with_loader<R>(constant: &str, f: impl FnOnce(&mut LoaderDecl) -> R) -> Option<R> {
    LOADERS.with(|l| l.borrow_mut().iter_mut().rev().find(|d| d.constant == constant).map(f))
}

fn is_loader(constant: &str) -> bool {
    LOADERS.with(|l| l.borrow().iter().any(|d| d.constant == constant))
}

/// Record a new loader. A second walk of the same file declares the
/// same one again: it restarts that declaration, so the calls the walk
/// applies next land on one record, not on a twin that is never set up.
fn declare(decl: LoaderDecl) {
    LOADERS.with(|l| {
        let mut loaders = l.borrow_mut();
        loaders.retain(|d| !(d.constant == decl.constant && d.root == decl.root && d.namespace == decl.namespace));
        loaders.push(decl);
    });
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

/// Declare the loader `value` creates under `key`. `None` when `value`
/// is not a `Zeitwerk::Loader` constructor; otherwise the value the
/// constant or local holds: the generated handle, or a refusal for a
/// form that is not modeled.
fn declare_loader(value: &Node<'_>, key: &str, file: &str) -> Option<Expr> {
    let call = value.as_call_node()?;
    if !is_zeitwerk_loader(call.receiver()) {
        return None;
    }
    let method = constant_id_str(&call.name());
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    if call.block().is_some() {
        return Some(refusal(value, file, format!("`Zeitwerk::Loader.{method}` with a block is not modeled")));
    }
    let calling_dir = || Path::new(file).parent().map(Path::to_path_buf);
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
                    return Some(refusal(
                        value,
                        file,
                        "`Zeitwerk::Loader.for_gem` argument other than a literal `warn_on_extra_files:` is not modeled"
                            .into(),
                    ));
                }
            }
            (calling_dir(), Vec::new())
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
                return Some(refusal(
                    value,
                    file,
                    "`Zeitwerk::Loader.for_gem_extension` of anything but a constant naming the namespace is not modeled"
                        .into(),
                ));
            };
            let namespace: Vec<String> = namespace.into_iter().filter(|s| !s.is_empty()).collect();
            (calling_dir(), namespace)
        }
        "new" if args.is_empty() => (None, Vec::new()),
        other => {
            return Some(refusal(value, file, format!("`Zeitwerk::Loader.{other}` is not modeled")));
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
    });
    Some(handle(key, span_of(value, file)))
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
    declare_loader(&write.value(), &constant, file)
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
    let key_of = |receiver: &Node<'_>| {
        receiver.as_constant_read_node().map(|name| qualified(owner, constant_id_str(&name.name())))
    };
    match apply(stmt, file, &key_of, false)? {
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
        let value = declare_loader(&write.value(), &key, file)?;
        let mut assign = super::expr::ingest_expr(stmt, file).ok()?;
        if let ExprNode::Assign { target: LValue::Var { .. }, value: written } = &mut *assign.node {
            *written = value;
        } else {
            return None;
        }
        return Some(Some(ProgramItem::Statement(assign)));
    }
    let key_of = |receiver: &Node<'_>| {
        if let Some(name) = receiver.as_constant_read_node() {
            return Some(constant_id_str(&name.name()).to_string());
        }
        receiver.as_local_variable_read_node().map(|local| local_key(constant_id_str(&local.name()), file))
    };
    Some(match apply(stmt, file, &key_of, true)? {
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
/// declared loader by `key_of`. `None` when `stmt` is not one.
fn apply(stmt: &Node<'_>, file: &str, key_of: &dyn Fn(&Node<'_>) -> Option<String>, file_level: bool) -> Option<Applied> {
    let outer = stmt.as_call_node()?;
    let mut call = stmt.as_call_node()?;
    let key = loop {
        let receiver = call.receiver()?;
        if let Some(key) = key_of(&receiver) {
            break key;
        }
        call = receiver.as_call_node()?;
    };
    if !is_loader(&key) {
        return None;
    }
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
        "push_dir" if args.len() == 1 => {
            let Some(path) = literal_path(&args[0], file) else {
                return Some(Applied::Refused(
                    "`Zeitwerk::Loader#push_dir` of a path that is not a literal under `__dir__` is not modeled".into(),
                ));
            };
            let second_root = with_loader(&key, |d| {
                let second = d.root.as_ref().is_some_and(|root| *root != path);
                d.root.get_or_insert(path);
                second
            });
            if second_root == Some(true) {
                return Some(Applied::Refused("a Zeitwerk loader with more than one root directory is not modeled".into()));
            }
            Some(Applied::Static)
        }
        "eager_load_dir" if args.len() == 1 && file_level => {
            let Some(path) = literal_path(&args[0], file) else {
                return Some(Applied::Refused(
                    "`Zeitwerk::Loader#eager_load_dir` of a path that is not a literal under `__dir__` is not modeled"
                        .into(),
                ));
            };
            let decl = LOADERS.with(|l| l.borrow().iter().rev().find(|d| d.constant == key).cloned())?;
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
    let Some(root) = decl.root.as_ref() else {
        return Applied::Refused("`Zeitwerk::Loader#eager_load_dir` on a loader with no root directory is not modeled".into());
    };
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
        decl.namespace.iter().cloned().chain(segments.iter().map(|s| camelize(s, &decl.inflections))).collect();
    // The constant the calling file itself defines under this loader:
    // while Ruby requires the file, its autoload reads as undefined until
    // the file opens it, so an `eager_load_dir` under it (graphql-shopify's
    // `shopify/patches`, called before `module GraphQL::Shopify`) returns.
    let own: Option<Vec<String>> = Path::new(file).strip_prefix(root).ok().and_then(|rel| {
        let rel = rel.with_extension("");
        let parts: Vec<String> =
            rel.components().map(|c| camelize(&c.as_os_str().to_string_lossy(), &decl.inflections)).collect();
        let own: Vec<String> = decl.namespace.iter().cloned().chain(parts).collect();
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
pub(super) fn resolve(decls: &[LoaderDecl], files: &[(PathBuf, Vec<ClassId>)]) -> Vec<crate::app::ZeitwerkLoader> {
    decls
        .iter()
        .filter_map(|decl| {
            let root = decl.root.as_ref()?;
            let mut managed = Vec::new();
            let mut ignored = Vec::new();
            let mut managed_files: Vec<(&PathBuf, &Vec<ClassId>)> = Vec::new();
            for (path, classes) in files {
                if !path.starts_with(root) {
                    continue;
                }
                let skip = !decl.setup || decl.ignores.iter().any(|ignore| path.starts_with(ignore));
                if skip {
                    ignored.extend(classes.iter().cloned());
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
            Some(crate::app::ZeitwerkLoader {
                constant: decl.constant.clone(),
                root: root.display().to_string(),
                managed,
                ignored,
                eager_dirs,
            })
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
