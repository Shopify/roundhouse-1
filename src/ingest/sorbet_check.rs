//! sorbet-runtime's class check for `T.let`, `T.cast`, `T.bind` and
//! `T.assert_type!`, as the Ruby trees run it.
//!
//! sorbet-runtime 0.6.13502 checks each of those calls at runtime: the
//! value passes when sorbet's `valid?` accepts it for the written type,
//! else the call raises `TypeError` ("T.let: Expected type String, got
//! type NilClass\nCaller: path:line"). No tree loads sorbet-runtime, so
//! the call lowers to the CRuby overlay's
//! `SorbetRuntimeCheck.cast(value, type, "T.let", path, line)`
//! (`runtime/spinel/scaffold/ruby_overlay/runtime/sorbet_runtime_check.rb`),
//! with the type argument rebuilt from the written type expression:
//!
//! - a constant is the Module itself (`is_a?`);
//! - `T.nilable(X)` / `T.any(A, B, ...)` are unions (any member passes,
//!   `NilClass` for the nil of `T.nilable`), `T::Boolean` is
//!   TrueClass or FalseClass;
//! - `T::Array[X]`, `T::Hash[K, V]`, `T::Set`, `T::Range`,
//!   `T::Enumerable`, `T::Enumerator` (and its `Lazy` / `Chain`),
//!   `T::Class`, `T::Module` check the outer class only, as sorbet does;
//!   their arguments only spell the message;
//! - a user generic `Foo[X]` is `Foo`: `T::Generic#[]` answers `self`;
//! - `T.class_of(C)` is `C.singleton_class`; `T.untyped` / `T.anything`
//!   accept everything.
//!
//! A form sorbet checks some other way, or that names sorbet-runtime's
//! own constants (`T.proc`, `T.self_type`, `T.attached_class`,
//! `T.type_parameter`, `T.all`, shapes, ...), is refused at the site by
//! name ("sorbet type check"), never skipped. A check that provably
//! passes (`T.untyped`, `nil` into a nilable type, a literal `[]` into
//! `T::Array[...]`, ...) is the value alone, as is `checked: false`.
//! `project` refuses the lowered call on every tree but CRuby / JRuby.

use ruby_prism::Node;

use crate::Symbol;
use crate::expr::{Expr, ExprNode, Literal};
use crate::span::Span;

use super::util::constant_id_str;
use super::IngestResult;

/// The construct name a refused check reports under.
pub(crate) const CONSTRUCT: &str = "sorbet type check";

/// The class every lowered check calls.
pub(crate) const HELPER: &str = "SorbetRuntimeCheck";

/// What a `T.let`-family call lowers to.
pub(super) enum Plan {
    /// The value alone: the check cannot fail, or `checked: false`.
    Value,
    /// `SorbetRuntimeCheck.cast(value, <type>, ...)`.
    Check(Expr),
    /// Refused by name, with the reason.
    Refuse(String),
}

/// A type expression, as far as the check reads it.
enum SorbetType {
    /// A constant the value must be `is_a?`.
    Module(Expr, Option<String>),
    Untyped,
    Anything,
    Boolean,
    Nilable(Box<SorbetType>),
    Any(Vec<SorbetType>),
    Generic { label: &'static str, outer: &'static [&'static str], args: Vec<SorbetType> },
    ClassOf(Expr),
}

impl SorbetType {
    /// Whether every value passes.
    fn always_valid(&self) -> bool {
        match self {
            SorbetType::Untyped | SorbetType::Anything => true,
            SorbetType::Nilable(inner) => inner.always_valid(),
            SorbetType::Any(members) => members.iter().any(SorbetType::always_valid),
            _ => false,
        }
    }

    /// The union members at the top, flattened.
    fn members(&self) -> Vec<&SorbetType> {
        match self {
            SorbetType::Nilable(inner) => inner.members(),
            SorbetType::Any(members) => members.iter().flat_map(SorbetType::members).collect(),
            other => vec![other],
        }
    }

    fn admits_nil(&self) -> bool {
        match self {
            SorbetType::Nilable(_) => true,
            SorbetType::Any(members) => members.iter().any(SorbetType::admits_nil),
            SorbetType::Module(_, Some(name)) => name == "NilClass",
            _ => false,
        }
    }
}

/// The written `T.<method>(value, type[, checked: ...])` as a plan.
/// `value_node` is the call's first argument.
pub(super) fn plan(call: &ruby_prism::CallNode<'_>, value_node: &Node<'_>, file: &str) -> IngestResult<Plan> {
    let method = constant_id_str(&call.name());
    let Some(arguments) = call.arguments() else {
        return Ok(Plan::Refuse(format!("`T.{method}` without a type argument")));
    };
    let args: Vec<Node<'_>> = arguments.arguments().iter().collect();
    let Some(type_node) = args.get(1) else {
        return Ok(Plan::Refuse(format!("`T.{method}` without a type argument")));
    };
    match args.get(2) {
        None => {}
        Some(options) if args.len() == 3 => match checked_option(options) {
            Some(false) => return Ok(Plan::Value),
            Some(true) => {}
            None => {
                return Ok(Plan::Refuse(format!(
                    "`T.{method}`'s options are not a literal `checked: true` / `checked: false`"
                )))
            }
        },
        Some(_) => return Ok(Plan::Refuse(format!("`T.{method}` with more than three arguments"))),
    }
    let ty = match read_type(type_node, file)? {
        Ok(ty) => ty,
        Err(reason) => return Ok(Plan::Refuse(format!("`T.{method}`'s type {reason}"))),
    };
    if ty.always_valid() || passes_statically(value_node, &ty) {
        return Ok(Plan::Value);
    }
    Ok(Plan::Check(type_expr(&ty)))
}

/// `checked: true` / `checked: false`, the one option sorbet takes.
fn checked_option(node: &Node<'_>) -> Option<bool> {
    let hash = node.as_keyword_hash_node()?;
    let elements: Vec<Node<'_>> = hash.elements().iter().collect();
    let [element] = elements.as_slice() else { return None };
    let assoc = element.as_assoc_node()?;
    let key = assoc.key();
    let key = key.as_symbol_node()?;
    if key.unescaped() != b"checked" {
        return None;
    }
    let value = assoc.value();
    if value.as_true_node().is_some() {
        Some(true)
    } else if value.as_false_node().is_some() {
        Some(false)
    } else {
        None
    }
}

/// A literal whose class the type admits at the top: the check cannot
/// fail, so it is not emitted. (Natively the type expression is still
/// evaluated; sorbet's static check has already resolved its constants.)
fn passes_statically(value: &Node<'_>, ty: &SorbetType) -> bool {
    if value.as_nil_node().is_some() {
        return ty.admits_nil();
    }
    let members = ty.members();
    let outer = |label: &str| members.iter().any(|m| matches!(m, SorbetType::Generic { label: l, .. } if *l == label));
    if value.as_array_node().is_some() {
        return outer("T::Array");
    }
    if value.as_hash_node().is_some() {
        return outer("T::Hash");
    }
    if value.as_true_node().is_some() || value.as_false_node().is_some() {
        return members.iter().any(|m| matches!(m, SorbetType::Boolean));
    }
    false
}

/// The stdlib generics, by their `T::` path: the label their name
/// prints, and the outer class `valid?` tests.
const GENERICS: &[(&[&str], &str, &[&str], usize)] = &[
    (&["Array"], "T::Array", &["Array"], 1),
    (&["Hash"], "T::Hash", &["Hash"], 2),
    (&["Set"], "T::Set", &["Set"], 1),
    (&["Range"], "T::Range", &["Range"], 1),
    (&["Enumerable"], "T::Enumerable", &["Enumerable"], 1),
    (&["Enumerator"], "T::Enumerator", &["Enumerator"], 1),
    (&["Enumerator", "Lazy"], "T::Enumerator::Lazy", &["Enumerator", "Lazy"], 1),
    (&["Enumerator", "Chain"], "T::Enumerator::Chain", &["Enumerator", "Chain"], 1),
    (&["Class"], "T::Class", &["Class"], 1),
    (&["Module"], "T::Module", &["Module"], 1),
];

/// `T` or `::T`.
fn is_t(node: &Node<'_>) -> bool {
    if let Some(read) = node.as_constant_read_node() {
        return constant_id_str(&read.name()) == "T";
    }
    node.as_constant_path_node().is_some_and(|path| {
        path.parent().is_none() && path.name().is_some_and(|name| constant_id_str(&name) == "T")
    })
}

/// The segments after a leading `T::` (or `::T::`), when the path has one.
fn under_t(node: &Node<'_>) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = node.as_constant_path_node()?;
    loop {
        segments.push(constant_id_str(&current.name()?).to_string());
        let parent = current.parent()?;
        if is_t(&parent) {
            segments.reverse();
            return Some(segments);
        }
        current = parent.as_constant_path_node()?;
    }
}

/// The type expression `node`, or why the check cannot be built here.
fn read_type(node: &Node<'_>, file: &str) -> IngestResult<Result<SorbetType, String>> {
    let text = || String::from_utf8_lossy(node.location().as_slice()).into_owned();
    if node.as_constant_read_node().is_some() || node.as_constant_path_node().is_some() {
        if is_t(node) {
            return Ok(Err("`T` is not a type".into()));
        }
        if let Some(segments) = under_t(node) {
            if segments == ["Boolean"] {
                return Ok(Ok(SorbetType::Boolean));
            }
            return Ok(Err(format!(
                "`{}` names sorbet-runtime's own constant, which no tree loads",
                text()
            )));
        }
        let name = super::util::constant_path_segments_strs(node)
            .map(|segments| segments.join("::"));
        return Ok(Ok(SorbetType::Module(super::expr::ingest_expr(node, file)?, name)));
    }
    let Some(call) = node.as_call_node() else {
        let shape = if node.as_array_node().is_some() {
            "is a tuple shape, which sorbet checks member by member"
        } else if node.as_hash_node().is_some() {
            "is a hash shape, which sorbet checks key by key"
        } else {
            "is not a type expression this check reads"
        };
        return Ok(Err(format!("`{}` {shape}", text())));
    };
    let method = constant_id_str(&call.name());
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let Some(receiver) = call.receiver() else {
        return Ok(Err(format!("`{}` is not a type expression this check reads", text())));
    };
    if rooted_at_t_proc(&call) {
        return Ok(Err(format!("`{}` is a proc type, which sorbet checks by arity and signature", text())));
    }
    if call.block().is_some() {
        return Ok(Err(format!("`{}` takes a block, which no type this check reads does", text())));
    }
    if is_t(&receiver) {
        return Ok(match (method, args.as_slice()) {
            ("untyped", []) => Ok(SorbetType::Untyped),
            ("anything", []) => Ok(SorbetType::Anything),
            ("nilable", [inner]) => match read_type(inner, file)? {
                Ok(inner) => Ok(SorbetType::Nilable(Box::new(inner))),
                Err(reason) => Err(reason),
            },
            ("any", members) if members.len() >= 2 => {
                let mut out = Vec::with_capacity(members.len());
                for member in members {
                    match read_type(member, file)? {
                        Ok(member) => out.push(member),
                        Err(reason) => return Ok(Err(reason)),
                    }
                }
                Ok(SorbetType::Any(out))
            }
            ("class_of", [class]) if class.as_constant_read_node().is_some() || class.as_constant_path_node().is_some() => {
                if is_t(class) || under_t(class).is_some() {
                    Err(format!("`{}` names sorbet-runtime's own constant, which no tree loads", text()))
                } else {
                    Ok(SorbetType::ClassOf(super::expr::ingest_expr(class, file)?))
                }
            }
            ("proc", _) => Err(format!("`{}` is a proc type, which sorbet checks by arity and signature", text())),
            ("self_type", _) => Err(format!("`{}` is `T.self_type`, which depends on the receiver", text())),
            ("attached_class", _) => Err(format!("`{}` is `T.attached_class`, which depends on the receiver", text())),
            ("type_parameter", _) => Err(format!("`{}` is a method type parameter, which sorbet-runtime does not erase the same way", text())),
            ("all", _) => Err(format!("`{}` is an intersection", text())),
            ("noreturn", _) => Err(format!("`{}` is `T.noreturn`", text())),
            ("enum" | "deprecated_enum", _) => Err(format!("`{}` is a value enumeration", text())),
            _ => Err(format!("`{}` is not a type expression this check reads", text())),
        });
    }
    if method != "[]" {
        return Ok(Err(format!("`{}` is not a type expression this check reads", text())));
    }
    if let Some(segments) = under_t(&receiver) {
        let Some((_, label, outer, arity)) = GENERICS.iter().find(|(path, ..)| segments.iter().map(String::as_str).eq(path.iter().copied())) else {
            return Ok(Err(format!("`{}` names sorbet-runtime's own constant, which no tree loads", text())));
        };
        if args.len() != *arity {
            return Ok(Err(format!("`{}` takes {arity} type argument(s)", text())));
        }
        let mut read = Vec::with_capacity(args.len());
        for arg in &args {
            match read_type(arg, file)? {
                Ok(arg) => read.push(arg),
                Err(reason) => return Ok(Err(reason)),
            }
        }
        return Ok(Ok(SorbetType::Generic { label, outer, args: read }));
    }
    if receiver.as_constant_read_node().is_some() || receiver.as_constant_path_node().is_some() {
        // `T::Generic#[]` is `self`: the arguments are not evaluated
        // into anything the check reads.
        if is_t(&receiver) {
            return Ok(Err(format!("`{}` is not a type expression this check reads", text())));
        }
        let name = super::util::constant_path_segments_strs(&receiver).map(|segments| segments.join("::"));
        return Ok(Ok(SorbetType::Module(super::expr::ingest_expr(&receiver, file)?, name)));
    }
    Ok(Err(format!("`{}` is not a type expression this check reads", text())))
}

/// `T.proc`, or a builder chain on it (`T.proc.params(...).returns(...)`).
fn rooted_at_t_proc(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(receiver) = call.receiver() else { return false };
    if is_t(&receiver) {
        return constant_id_str(&call.name()) == "proc";
    }
    receiver.as_call_node().is_some_and(|inner| rooted_at_t_proc(&inner))
}

fn helper() -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Const { path: vec![Symbol::from(HELPER)] })
}

fn send(recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: Some(recv), method: Symbol::from(method), args, block: None, parenthesized: true },
    )
}

fn str_lit(span: Span, value: &str) -> Expr {
    Expr::new(span, ExprNode::Lit { value: Literal::Str { value: value.to_string() } })
}

/// The runtime type object the overlay's helper reads.
fn type_expr(ty: &SorbetType) -> Expr {
    match ty {
        SorbetType::Module(expr, _) => expr.clone(),
        SorbetType::Untyped => send(helper(), "untyped", Vec::new()),
        SorbetType::Anything => send(helper(), "anything", Vec::new()),
        SorbetType::Boolean => send(helper(), "boolean", Vec::new()),
        SorbetType::Nilable(inner) => send(helper(), "nilable", vec![type_expr(inner)]),
        SorbetType::Any(members) => send(helper(), "any", members.iter().map(type_expr).collect()),
        SorbetType::Generic { label, outer, args } => {
            let mut path = vec![Symbol::from("")];
            path.extend(outer.iter().map(|s| Symbol::from(*s)));
            let outer = Expr::new(Span::synthetic(), ExprNode::Const { path });
            let mut all = vec![str_lit(Span::synthetic(), label), outer];
            all.extend(args.iter().map(type_expr));
            send(helper(), "generic", all)
        }
        SorbetType::ClassOf(expr) => send(helper(), "class_of", vec![expr.clone()]),
    }
}

/// `SorbetRuntimeCheck.cast(value, type, "T.let", <source path>, line)`.
/// The path is the source file's, anchored on the emitted file as
/// `__FILE__` is (`SOURCE_FILE_PATH`), so the `Caller:` line names the
/// file the call was written in, as it does natively. `None` when the
/// call has no source file or line to name.
pub(super) fn checked_cast(
    span: Span,
    call: &ruby_prism::CallNode<'_>,
    value: Expr,
    ty: Expr,
    file: &str,
) -> Option<Expr> {
    if file.starts_with('<') {
        return None;
    }
    let message = call.message_loc().unwrap_or_else(|| call.location());
    let line = super::sources::line_at_parse(&message)
        .or_else(|| super::sources::line_at(file, message.start_offset()))?;
    let method = format!("T.{}", constant_id_str(&call.name()));
    let path = super::expr::source_file_path_lit(span, super::sources::relative_path(file));
    let line = Expr::new(span, ExprNode::Lit { value: Literal::Int { value: line as i64 } });
    let mut cast = send(helper(), "cast", vec![value, ty, str_lit(span, &method), path, line]);
    cast.span = span;
    Some(cast)
}

/// Whether `expr` is a lowered check (`project` refuses it off the
/// CRuby trees).
pub(crate) fn is_check(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Send { recv: Some(recv), method, .. }
        if method.as_str() == "cast"
            && matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == HELPER))
}

/// A refused check's diagnostic.
pub(super) fn refusal(detail: String) -> crate::diagnostic::DiagnosticKind {
    crate::diagnostic::DiagnosticKind::Unsupported {
        target: None,
        construct: Symbol::from(CONSTRUCT),
        detail,
    }
}
