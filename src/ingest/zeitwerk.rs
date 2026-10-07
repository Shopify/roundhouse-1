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
//! The emitted tree does not run Zeitwerk. The gem's `lib/` is already a
//! support root, so its files are carried and the generated
//! `app/models.rb` registers each one's constant with `Module#autoload`,
//! Zeitwerk's own model (`project::apply_models_autoload`). What the
//! loader adds is read here, statically: its root directory (the calling
//! file's directory for `for_gem`, the `push_dir` argument for
//! `Loader.new`), the paths it ignores, and whether it is set up. A file
//! under an ignored path, or under a loader that is never set up, is not
//! autoloaded or eager-loaded, exactly as Zeitwerk leaves it alone. The
//! constant itself becomes the generated `RoundhouseAutoload.loader`
//! handle, whose `eager_load` loads the loader's files.
//!
//! Paths are evaluated at compile time from literals:
//! `File.join(__dir__, "x", …)`, `File.expand_path("x", __dir__)` and
//! `"#{__dir__}/x"` are the source file's directory joined with the
//! literal parts. Default inflection only. Any other loader call (a custom
//! inflector, `collapse`, `eager_load` in the body, reloading, callbacks,
//! a computed path) is a named refusal at its site, which raises
//! there when the class body runs.
//!
//! Recorded through a thread-local collector like [`super::survey`],
//! since a class-body walk returns classes, not app facts;
//! `ingest::app` drains it after the support walk and resolves the
//! carried files against each loader.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use ruby_prism::Node;

use crate::Symbol;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::ClassId;
use crate::span::Span;

use super::util::{constant_id_str, constant_path_of, string_value};

/// One loader a class body declared.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LoaderDecl {
    /// The constant holding it (`Functions::ZeitwerkLoader`).
    pub constant: String,
    /// The directory it manages, absolute like the walk's entries.
    pub root: Option<PathBuf>,
    /// Paths (file or directory) it ignores, absolute.
    pub ignores: Vec<PathBuf>,
    /// Whether the body calls `setup`.
    pub setup: bool,
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

fn with_loader(constant: &str, f: impl FnOnce(&mut LoaderDecl)) {
    LOADERS.with(|l| {
        if let Some(decl) = l.borrow_mut().iter_mut().rev().find(|d| d.constant == constant) {
            f(decl);
        }
    });
}

fn is_loader(constant: &str) -> bool {
    LOADERS.with(|l| l.borrow().iter().any(|d| d.constant == constant))
}

fn qualified(owner: &ClassId, name: &str) -> String {
    if owner.0.as_str().is_empty() { name.to_string() } else { format!("{}::{name}", owner.0.as_str()) }
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
    let arg = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: constant.to_string() } });
    let call = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from("loader"),
            args: vec![arg],
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

/// `NAME = Zeitwerk::Loader.for_gem` / `Zeitwerk::Loader.new` in a class
/// body. `None` when the value is not a loader. Otherwise the value the
/// constant holds: the generated handle, or a refusal for a form that is
/// not modeled.
pub(super) fn loader_constant(
    write: &ruby_prism::ConstantWriteNode<'_>,
    owner: &ClassId,
    file: &str,
) -> Option<Expr> {
    let value = write.value();
    let call = value.as_call_node()?;
    if !is_zeitwerk_loader(call.receiver()) {
        return None;
    }
    let constant = qualified(owner, constant_id_str(&write.name()));
    let method = constant_id_str(&call.name());
    let root = match method {
        // `for_gem` manages the calling file's directory.
        "for_gem" => {
            if call.block().is_some() {
                return Some(refusal(&value, file, "`Zeitwerk::Loader.for_gem` with a block is not modeled".into()));
            }
            // `warn_on_extra_files:` only decides a warning at setup.
            for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
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
                        &value,
                        file,
                        "`Zeitwerk::Loader.for_gem` argument other than a literal `warn_on_extra_files:` is not modeled"
                            .into(),
                    ));
                }
            }
            Path::new(file).parent().map(Path::to_path_buf)
        }
        "new" if call.arguments().is_none() && call.block().is_none() => None,
        other => {
            return Some(refusal(&value, file, format!("`Zeitwerk::Loader.{other}` is not modeled")));
        }
    };
    LOADERS.with(|l| l.borrow_mut().push(LoaderDecl { constant: constant.clone(), root, ignores: Vec::new(), setup: false }));
    Some(handle(&constant, span_of(&value, file)))
}

/// A class-body call on a loader this body declared (`NAME.setup`,
/// `NAME.ignore(…)`, `NAME.push_dir(…)`). `None` when `stmt` is not
/// one. `Some(None)` when it is applied statically; `Some(Some(refusal))`
/// for a loader call that is not modeled, to keep at its site.
pub(super) fn loader_call(stmt: &Node<'_>, owner: &ClassId, file: &str) -> Option<Option<Expr>> {
    // The call on the loader itself: the innermost of a chain
    // (`ZeitwerkLoader.inflector.inflect(…)` calls `inflector` on it).
    let mut call = stmt.as_call_node()?;
    let name = loop {
        let receiver = call.receiver()?;
        if let Some(name) = receiver.as_constant_read_node() {
            break name;
        }
        call = receiver.as_call_node()?;
    };
    let constant = qualified(owner, constant_id_str(&name.name()));
    if !is_loader(&constant) {
        return None;
    }
    let chained = call.location().end_offset() != stmt.location().end_offset();
    let method = constant_id_str(&call.name());
    if chained {
        return Some(Some(refusal(stmt, file, format!("`Zeitwerk::Loader#{method}` is not modeled"))));
    }
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let refuse = |detail: String| Some(Some(refusal(stmt, file, detail)));
    if call.block().is_some() {
        return refuse(format!("`Zeitwerk::Loader#{method}` with a block is not modeled"));
    }
    match method {
        "setup" if args.is_empty() => {
            with_loader(&constant, |d| d.setup = true);
            Some(None)
        }
        "ignore" => {
            let mut paths = Vec::new();
            for arg in &args {
                match literal_path(arg, file) {
                    Some(path) => paths.push(path),
                    None => {
                        return refuse(
                            "`Zeitwerk::Loader#ignore` of a path that is not `File.join(__dir__, …)` or another \
                             literal under `__dir__` is not modeled"
                                .into(),
                        );
                    }
                }
            }
            with_loader(&constant, |d| d.ignores.extend(paths));
            Some(None)
        }
        "push_dir" if args.len() == 1 => {
            let Some(path) = literal_path(&args[0], file) else {
                return refuse(
                    "`Zeitwerk::Loader#push_dir` of a path that is not a literal under `__dir__` is not modeled".into(),
                );
            };
            let mut second_root = false;
            with_loader(&constant, |d| {
                second_root = d.root.as_ref().is_some_and(|root| *root != path);
                d.root.get_or_insert(path);
            });
            if second_root {
                return refuse("a Zeitwerk loader with more than one root directory is not modeled".into());
            }
            Some(None)
        }
        other => refuse(format!("`Zeitwerk::Loader#{other}` is not modeled")),
    }
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
            for (path, classes) in files {
                if !path.starts_with(root) {
                    continue;
                }
                let skip = !decl.setup || decl.ignores.iter().any(|ignore| path.starts_with(ignore));
                if skip { &mut ignored } else { &mut managed }.extend(classes.iter().cloned());
            }
            Some(crate::app::ZeitwerkLoader {
                constant: decl.constant.clone(),
                root: root.display().to_string(),
                managed,
                ignored,
            })
        })
        .collect()
}
