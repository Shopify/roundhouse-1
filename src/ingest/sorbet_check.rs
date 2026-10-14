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
//! - a proc type `T.proc.params(a: X).returns(Y)` (or `.void`, with an
//!   optional `.bind(Z)`) is sorbet's `T::Types::Proc`, whose `valid?`
//!   is `obj.is_a?(::Proc)`: the parameter and return types only spell
//!   the message (`T.proc.params(a: X).returns(Y)`, `<VOID>` for
//!   `void`), at the top as in a generic's arguments
//!   (`T::Hash[Symbol, T.proc.returns(T.untyped)]`);
//! - a user generic `Foo[X]` is `Foo`: `T::Generic#[]` answers `self`;
//! - `T.class_of(C)` is `C.singleton_class`; `T.untyped` / `T.anything`
//!   accept everything;
//! - `T.all(A, B, ...)` is sorbet's `Intersection`: every member passes;
//! - a hash shape `{a: X, "b" => Y}` is sorbet's `FixedHash`: a Hash
//!   whose every declared key's value passes its type (an absent key
//!   reads nil) and which has no other key;
//! - a tuple shape `[A, B]` is sorbet's `FixedArray`: an Array of exactly
//!   that length whose every element passes the type at its position.
//!
//! A form sorbet checks some other way, or that names sorbet-runtime's
//! own constants (`T.self_type`, `T.attached_class`, `T.type_parameter`,
//! a splat in a tuple shape, `T::Sig::WithoutRuntime`, ...), is refused
//! at the site by name ("sorbet type check"), never skipped. A check that
//! provably passes (`T.untyped`, `nil` into a nilable type, a literal `[]` into
//! `T::Array[...]`, a `->(x) { ... }` literal into a proc type, ...) is the
//! value alone, as is `checked: false`.
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
    /// `T.all(A, B, ...)`: sorbet's `T::Types::Intersection`, which
    /// passes a value every member passes.
    All(Vec<SorbetType>),
    Generic { label: &'static str, outer: &'static [&'static str], args: Vec<SorbetType> },
    ClassOf(Expr),
    /// `{a: Integer, "b" => String}`: sorbet's `T::Types::FixedHash`,
    /// which checks each declared key's value and admits no other key.
    FixedHash(Vec<(ShapeKey, SorbetType)>),
    /// `[Integer, String]`: sorbet's `T::Types::FixedArray`, an Array
    /// of that length whose each element passes its position's type.
    FixedArray(Vec<SorbetType>),
    /// `T.proc.params(a: X).returns(Y)` (`None` for `.void`): sorbet's
    /// `T::Types::Proc`, which passes any Proc; the parameter and
    /// return types are read for its name.
    Proc { params: Vec<(String, SorbetType)>, returns: Option<Box<SorbetType>> },
}

/// A shape's key, as written: sorbet keys the hash by the literal.
enum ShapeKey {
    Sym(String),
    Str(String),
}

impl SorbetType {
    /// Whether every value passes.
    fn always_valid(&self) -> bool {
        match self {
            SorbetType::Untyped | SorbetType::Anything => true,
            SorbetType::Nilable(inner) => inner.always_valid(),
            SorbetType::Any(members) => members.iter().any(SorbetType::always_valid),
            SorbetType::All(members) => members.iter().all(SorbetType::always_valid),
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
            SorbetType::All(members) => members.iter().all(SorbetType::admits_nil),
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
    // `->(x) { ... }` is a Proc, which is all `T::Types::Proc#valid?`
    // asks (hedwig's `T.let(->(_job) { false }, T.proc.params(job:
    // Hedwig::Job).returns(T::Boolean))`).
    if value.as_lambda_node().is_some() {
        return members.iter().any(|m| matches!(m, SorbetType::Proc { .. }));
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
    read_type_at(node, file, false)
}

/// `read_type`; `named` inside a stdlib generic's or a proc type's
/// arguments, which sorbet's `valid?` never checks (`TypedHash#valid?`
/// is `obj.is_a?(Hash)`, `T::Types::Proc#valid?` `obj.is_a?(Proc)`), so
/// there a type is read for its name alone.
fn read_type_at(node: &Node<'_>, file: &str, named: bool) -> IngestResult<Result<SorbetType, String>> {
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
    if let Some(hash) = node.as_hash_node() {
        return read_shape(&hash, file, named);
    }
    if let Some(array) = node.as_array_node() {
        return read_tuple(&array, file, named);
    }
    let Some(call) = node.as_call_node() else {
        return Ok(Err(format!("`{}` is not a type expression this check reads", text())));
    };
    let method = constant_id_str(&call.name());
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let Some(receiver) = call.receiver() else {
        return Ok(Err(format!("`{}` is not a type expression this check reads", text())));
    };
    if rooted_at_t_proc(&call) {
        return read_proc(&call, file);
    }
    if call.block().is_some() {
        return Ok(Err(format!("`{}` takes a block, which no type this check reads does", text())));
    }
    if is_t(&receiver) {
        return Ok(match (method, args.as_slice()) {
            ("untyped", []) => Ok(SorbetType::Untyped),
            ("anything", []) => Ok(SorbetType::Anything),
            ("nilable", [inner]) => match read_type_at(inner, file, named)? {
                Ok(inner) => Ok(SorbetType::Nilable(Box::new(inner))),
                Err(reason) => Err(reason),
            },
            ("any", members) if members.len() >= 2 => {
                let mut out = Vec::with_capacity(members.len());
                for member in members {
                    match read_type_at(member, file, named)? {
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
            ("self_type", _) => Err(format!("`{}` is `T.self_type`, which depends on the receiver", text())),
            ("attached_class", _) => Err(format!("`{}` is `T.attached_class`, which depends on the receiver", text())),
            ("type_parameter", _) => Err(format!("`{}` is a method type parameter, which sorbet-runtime does not erase the same way", text())),
            ("all", members) if members.len() >= 2 => {
                let mut out = Vec::with_capacity(members.len());
                for member in members {
                    match read_type_at(member, file, named)? {
                        Ok(member) => out.push(member),
                        Err(reason) => return Ok(Err(reason)),
                    }
                }
                Ok(SorbetType::All(out))
            }
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
            match read_type_at(arg, file, true)? {
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

/// A hash shape: each key a plain symbol or string literal (what sorbet
/// keys `FixedHash#types` by), each value a type this check reads.
fn read_shape(hash: &ruby_prism::HashNode<'_>, file: &str, named: bool) -> IngestResult<Result<SorbetType, String>> {
    let text = || String::from_utf8_lossy(hash.location().as_slice()).into_owned();
    let mut fields = Vec::new();
    for element in hash.elements().iter() {
        let Some(assoc) = element.as_assoc_node() else {
            return Ok(Err(format!("`{}` splats a hash into a shape", text())));
        };
        let key = assoc.key();
        let key = if let Some(sym) = key.as_symbol_node() {
            ShapeKey::Sym(String::from_utf8_lossy(sym.unescaped()).into_owned())
        } else if let Some(string) = key.as_string_node() {
            ShapeKey::Str(String::from_utf8_lossy(string.unescaped()).into_owned())
        } else {
            return Ok(Err(format!("`{}` has a key that is not a symbol or string literal", text())));
        };
        match read_type_at(&assoc.value(), file, named)? {
            Ok(ty) => fields.push((key, ty)),
            Err(reason) => return Ok(Err(reason)),
        }
    }
    Ok(Ok(SorbetType::FixedHash(fields)))
}

/// A tuple shape: each element a type this check reads, in place (a
/// splat would make the length the check fixes a runtime value).
fn read_tuple(array: &ruby_prism::ArrayNode<'_>, file: &str, named: bool) -> IngestResult<Result<SorbetType, String>> {
    let mut types = Vec::new();
    for element in array.elements().iter() {
        if element.as_splat_node().is_some() {
            let text = String::from_utf8_lossy(array.location().as_slice()).into_owned();
            return Ok(Err(format!("`{text}` splats an array into a tuple shape")));
        }
        match read_type_at(&element, file, named)? {
            Ok(ty) => types.push(ty),
            Err(reason) => return Ok(Err(reason)),
        }
    }
    Ok(Ok(SorbetType::FixedArray(types)))
}

/// A proc type: sorbet-runtime's `T.proc` builder
/// (`private/methods/decl_builder.rb`) takes `bind`, `params` (keywords
/// only) and then `returns(X)` or `void`; `T::Types::Proc#valid?` is
/// `obj.is_a?(::Proc)` and its `name` spells `T.proc.params(a:
/// X).returns(Y)` (`<VOID>` for `void`; the bind is not in the name).
fn read_proc(call: &ruby_prism::CallNode<'_>, file: &str) -> IngestResult<Result<SorbetType, String>> {
    /// Each builder step, innermost (`proc`) first: its name, its
    /// arguments and whether it takes a block; `false` when the chain is
    /// not rooted at `T`.
    fn steps<'pr>(call: &ruby_prism::CallNode<'pr>, out: &mut Vec<(String, Vec<Node<'pr>>, bool)>) -> bool {
        let Some(receiver) = call.receiver() else { return false };
        if !is_t(&receiver) {
            let Some(inner) = receiver.as_call_node() else { return false };
            if !steps(&inner, out) {
                return false;
            }
        }
        let args = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        out.push((constant_id_str(&call.name()).to_string(), args, call.block().is_some()));
        true
    }
    let text = String::from_utf8_lossy(call.location().as_slice()).into_owned();
    let unread = || Ok(Err(format!("`{text}` is a proc type this check does not read")));
    let mut chain = Vec::new();
    if !steps(call, &mut chain) {
        return unread();
    }
    let mut params: Option<Vec<(String, SorbetType)>> = None;
    let mut returns: Option<Option<Box<SorbetType>>> = None;
    let mut bound = false;
    for (i, (name, args, block)) in chain.iter().enumerate() {
        if *block || returns.is_some() {
            return unread();
        }
        match (name.as_str(), args.as_slice()) {
            ("proc", []) if i == 0 => {}
            ("bind", [_]) if i > 0 && !bound && params.is_none() => bound = true,
            ("params", [keywords]) if i > 0 && params.is_none() => {
                let Some(hash) = keywords.as_keyword_hash_node() else { return unread() };
                let mut read = Vec::new();
                for element in hash.elements().iter() {
                    let Some(assoc) = element.as_assoc_node() else { return unread() };
                    let Some(key) = assoc.key().as_symbol_node().map(|k| String::from_utf8_lossy(k.unescaped()).into_owned()) else {
                        return unread();
                    };
                    match read_type_at(&assoc.value(), file, true)? {
                        Ok(ty) => read.push((key, ty)),
                        Err(reason) => return Ok(Err(reason)),
                    }
                }
                params = Some(read);
            }
            ("returns", [ty]) if i > 0 => match read_type_at(ty, file, true)? {
                Ok(ty) => returns = Some(Some(Box::new(ty))),
                Err(reason) => return Ok(Err(reason)),
            },
            ("void", []) if i > 0 => returns = Some(None),
            _ => return unread(),
        }
    }
    let Some(returns) = returns else {
        return Ok(Err(format!("`{text}` is a proc type without `returns` or `void`, which sorbet rejects")));
    };
    Ok(Ok(SorbetType::Proc { params: params.unwrap_or_default(), returns }))
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

/// `Name = T.type_alias { X }` as the Ruby trees carry it:
/// `SorbetRuntimeCheck.type_alias { <X> }`, a type object whose block
/// runs at its first check, as sorbet-runtime's `aliased_type` does, so
/// a check naming the alias (`T.let(x, Name)`) reaches the type it
/// names. A `T.type_alias` whose type the check does not read carries a
/// block that raises that refusal, so a check reading it fails by name
/// and an annotation reads it as untyped. `None` when `value` is not
/// that call, or for a type parameter whose `fixed:` type is unread;
/// such a parameter goes, as before, with the `sig`s that read it.
/// `project` drops the carried alias off the CRuby / JRuby trees, which
/// refuse every check that could read it.
///
/// A generic's type parameter (`OkType = type_member(:out)`, shopify-adt's
/// `ResultWrapper`) is read the same way, as the type a `sig` naming it
/// may assume: the `fixed:` type when it declares one, else `T.untyped`,
/// which is also what sorbet-runtime checks for it (nothing).
pub(crate) fn alias_value(value: &Node<'_>, file: &str) -> IngestResult<Option<Expr>> {
    let Some(call) = value.as_call_node() else { return Ok(None) };
    let body = if matches!(constant_id_str(&call.name()), "type_member" | "type_template") && call.receiver().is_none() {
        type_expr(&match type_parameter_fixed(&call) {
            Some(fixed) => match read_type(&fixed, file)? {
                Ok(ty) => ty,
                Err(_) => return Ok(None),
            },
            None => SorbetType::Untyped,
        })
    } else {
        if constant_id_str(&call.name()) != "type_alias" || !call.receiver().is_some_and(|r| is_t(&r)) || call.arguments().is_some() {
            return Ok(None);
        }
        let Some(body) = call
            .block()
            .and_then(|b| b.as_block_node())
            .and_then(|b| b.body())
            .and_then(|b| b.as_statements_node())
        else {
            return Ok(None);
        };
        let statements: Vec<Node<'_>> = body.body().iter().collect();
        let [ty] = statements.as_slice() else { return Ok(None) };
        match read_type(ty, file)? {
            Ok(ty) => type_expr(&ty),
            // A member no check reads (`T::Enum` in Observe's
            // `AllowedValueType`): the alias is still a constant, so an
            // annotation naming it (`{} #: Hash[K, AllowedValueType]`,
            // never evaluated) reads it as untyped (`alias_ty`) rather
            // than refusing the authored value as an unknown constant.
            // A check that reads it raises here, by name.
            Err(reason) => {
                let loc = ty.location();
                let span = Span { file: super::sources::file_id(file), start: loc.start_offset() as u32, end: loc.end_offset() as u32 };
                let mut refused = Expr::new(span, ExprNode::Lit { value: Literal::Nil });
                refused.diagnostic = Some(refusal(reason));
                refused
            }
        }
    };
    let loc = value.location();
    let span = Span { file: super::sources::file_id(file), start: loc.start_offset() as u32, end: loc.end_offset() as u32 };
    let block = Expr::new(
        Span::synthetic(),
        ExprNode::Lambda { signature: Default::default(), extra_params: Vec::new(),
            params: Vec::new(),
            rest_param: None,
            block_param: None,
            body,
            block_style: crate::expr::BlockStyle::Brace,
        },
    );
    Ok(Some(Expr::new(
        span,
        ExprNode::Send { recv: Some(helper()), method: Symbol::from("type_alias"), args: Vec::new(), block: Some(block), parenthesized: false },
    )))
}

/// The `X` of `type_member { { fixed: X } }`; `None` for a parameter
/// without one (bounds only, or no block).
fn type_parameter_fixed<'pr>(call: &ruby_prism::CallNode<'pr>) -> Option<Node<'pr>> {
    let body = call.block()?.as_block_node()?.body()?.as_statements_node()?;
    let statements: Vec<Node<'pr>> = body.body().iter().collect();
    let [hash] = statements.as_slice() else { return None };
    let hash = hash.as_hash_node()?;
    hash.elements().iter().find_map(|element| {
        let assoc = element.as_assoc_node()?;
        let key = assoc.key().as_symbol_node()?;
        (key.unescaped() == b"fixed").then(|| assoc.value())
    })
}

/// The static type a carried alias (`alias_value`) names, read back from
/// its type object the way `sorbet_sig::sorbet_ty` reads the written
/// type: the analyzer substitutes it where a declared type names the
/// alias (`analyze::body::expand_type_aliases`). A user generic is its
/// class without arguments; `None` for anything else.
pub(crate) fn alias_ty(expr: &Expr) -> Option<crate::ty::Ty> {
    let ExprNode::Send { block: Some(block), .. } = &*expr.node else { return None };
    if !is_alias(expr) {
        return None;
    }
    let ExprNode::Lambda { body, .. } = &*block.node else { return None };
    if body.diagnostic.is_some() {
        // An alias no check reads (`alias_value`): untyped, as sorbet's
        // static reading of a member it cannot name would be.
        return Some(crate::ty::Ty::Untyped);
    }
    type_object_ty(body)
}

fn type_object_ty(expr: &Expr) -> Option<crate::ty::Ty> {
    use crate::ty::Ty;
    let const_name = |expr: &Expr| match &*expr.node {
        ExprNode::Const { path } => {
            Some(path.iter().map(Symbol::as_str).skip_while(|s| s.is_empty()).collect::<Vec<_>>().join("::"))
        }
        _ => None,
    };
    if let Some(name) = const_name(expr) {
        return Some(super::sorbet_sig::named_ty(&name));
    }
    let ExprNode::Send { recv: Some(recv), method, args, .. } = &*expr.node else { return None };
    if const_name(recv).as_deref() != Some(HELPER) {
        return None;
    }
    match (method.as_str(), args.as_slice()) {
        ("untyped" | "anything", []) => Some(Ty::Untyped),
        ("boolean", []) => Some(Ty::Bool),
        ("nilable", [inner]) => Some(Ty::Union { variants: vec![type_object_ty(inner)?, Ty::Nil].into() }),
        ("any", members) => Some(Ty::Union { variants: members.iter().map(type_object_ty).collect::<Option<_>>()? }),
        // The intersection `sorbet_sig` reads `T.all(A, B)` as.
        ("all", members) if members.len() >= 2 => {
            Some(crate::rbs::intersection_ty(members.iter().map(type_object_ty).collect::<Option<_>>()?))
        }
        ("class_of", [class]) => Some(Ty::Class { id: crate::ident::ClassId(Symbol::from(const_name(class)?.as_str())), args: Default::default() }),
        // A symbol-keyed shape is the record `sorbet_sig` reads `{a: X}`
        // as; one with a string key is the `T::Hash` sorbet widens a
        // shape to (`FixedHash#subtype_of_single?`), keys and values
        // each the union of theirs.
        ("fixed_hash", [hash]) => {
            let ExprNode::Hash { entries, .. } = &*hash.node else { return None };
            let mut fields = indexmap::IndexMap::new();
            let mut keys: Vec<Ty> = Vec::new();
            let mut values: Vec<Ty> = Vec::new();
            let mut all_symbols = true;
            for (key, value) in entries {
                let value = type_object_ty(value)?;
                let key_ty = match &*key.node {
                    ExprNode::Lit { value: Literal::Sym { value: key } } => {
                        fields.insert(key.clone(), value.clone());
                        Ty::Sym
                    }
                    ExprNode::Lit { value: Literal::Str { .. } } => {
                        all_symbols = false;
                        Ty::Str
                    }
                    _ => return None,
                };
                if !keys.contains(&key_ty) {
                    keys.push(key_ty);
                }
                if !values.contains(&value) {
                    values.push(value);
                }
            }
            if all_symbols {
                return Some(Ty::Record { row: crate::ty::Row { fields: fields.into(), rest: None } });
            }
            let union = |mut tys: Vec<Ty>| if tys.len() == 1 { tys.remove(0) } else { Ty::Union { variants: tys.into() } };
            Some(Ty::Hash { key: std::sync::Arc::new(union(keys)), value: std::sync::Arc::new(union(values)) })
        }
        // A tuple is the tuple `sorbet_sig` reads `[A, B]` as.
        ("fixed_array", [array]) => {
            let ExprNode::Array { elements, .. } = &*array.node else { return None };
            Some(Ty::Tuple { elems: elements.iter().map(type_object_ty).collect::<Option<_>>()? })
        }
        // A proc type is the `Ty::Fn` `sorbet_sig` reads `T.proc...` as.
        ("proc_type", [hash, ret]) => {
            let ExprNode::Hash { entries, .. } = &*hash.node else { return None };
            let mut params = Vec::new();
            for (key, value) in entries {
                let ExprNode::Lit { value: Literal::Sym { value: name } } = &*key.node else { return None };
                params.push(crate::ty::Param { name: name.clone(), ty: std::sync::Arc::new(type_object_ty(value)?), kind: crate::ty::ParamKind::Required });
            }
            let void = matches!(&*ret.node, ExprNode::Send { recv: Some(recv), method, args, .. }
                if method.as_str() == "void" && args.is_empty() && const_name(recv).as_deref() == Some(HELPER));
            let ret = if void { Ty::Nil } else { type_object_ty(ret)? };
            Some(Ty::Fn { params: params.into(), block: None, ret: std::sync::Arc::new(ret), effects: crate::effect::EffectSet::default() })
        }
        ("generic", [label, _outer, rest @ ..]) => {
            let ExprNode::Lit { value: Literal::Str { value: label } } = &*label.node else { return None };
            let args: Vec<Ty> = rest.iter().map(type_object_ty).collect::<Option<_>>()?;
            crate::rbs::sorbet_generic_ty(label, &args)
        }
        _ => None,
    }
}

/// Whether `expr` is a carried type alias (`alias_value`).
pub(crate) fn is_alias(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Send { recv: Some(recv), method, block: Some(_), .. }
        if method.as_str() == "type_alias"
            && matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == HELPER))
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
        SorbetType::All(members) => send(helper(), "all", members.iter().map(type_expr).collect()),
        SorbetType::Generic { label, outer, args } => {
            let mut path = vec![Symbol::from("")];
            path.extend(outer.iter().map(|s| Symbol::from(*s)));
            let outer = Expr::new(Span::synthetic(), ExprNode::Const { path });
            let mut all = vec![str_lit(Span::synthetic(), label), outer];
            all.extend(args.iter().map(type_expr));
            send(helper(), "generic", all)
        }
        SorbetType::ClassOf(expr) => send(helper(), "class_of", vec![expr.clone()]),
        SorbetType::FixedHash(fields) => {
            let entries = fields
                .iter()
                .map(|(key, ty)| {
                    let key = match key {
                        ShapeKey::Sym(name) => Literal::Sym { value: Symbol::from(name.as_str()) },
                        ShapeKey::Str(name) => Literal::Str { value: name.clone() },
                    };
                    (Expr::new(Span::synthetic(), ExprNode::Lit { value: key }), type_expr(ty))
                })
                .collect();
            let hash = Expr::new(Span::synthetic(), ExprNode::Hash { entries, kwargs: false });
            send(helper(), "fixed_hash", vec![hash])
        }
        SorbetType::FixedArray(types) => {
            let elements = types.iter().map(type_expr).collect();
            let array = Expr::new(Span::synthetic(), ExprNode::Array { elements, style: Default::default() });
            send(helper(), "fixed_array", vec![array])
        }
        SorbetType::Proc { params, returns } => {
            let entries = params
                .iter()
                .map(|(key, ty)| {
                    let key = Literal::Sym { value: Symbol::from(key.as_str()) };
                    (Expr::new(Span::synthetic(), ExprNode::Lit { value: key }), type_expr(ty))
                })
                .collect();
            let hash = Expr::new(Span::synthetic(), ExprNode::Hash { entries, kwargs: false });
            let returns = match returns {
                Some(ty) => type_expr(ty),
                None => send(helper(), "void", Vec::new()),
            };
            send(helper(), "proc_type", vec![hash, returns])
        }
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

/// `mixes_in_class_methods(A, ...)` written in a module or class body,
/// as `SorbetRuntimeCheck.mixes_in_class_methods(self, A, ...)`.
///
/// `T::Helpers#mixes_in_class_methods` is behavior, not an annotation:
/// each later `include` of the module extends the includer with `A`.
/// GraphQL interfaces (`GraphQL::Shopify::Types::BaseInterface`) get
/// their `definition_methods` onto every implementing type through it.
/// `extend T::Helpers` stays dropped with the other annotation mixins;
/// the CRuby overlay's sorbet_runtime_check.rb ports the two modules
/// that answer the call. `None` for any other call, or one with no
/// argument.
pub(super) fn mixes_in_class_methods(stmt: &Node<'_>, file: &str) -> Option<Expr> {
    let call = stmt.as_call_node()?;
    if call.receiver().is_some() || call.block().is_some() || constant_id_str(&call.name()) != MIXES_IN {
        return None;
    }
    if call.arguments().is_none_or(|a| a.arguments().iter().next().is_none()) {
        return None;
    }
    let lowered = super::expr::ingest_expr(stmt, file).ok()?;
    let span = lowered.span;
    let ExprNode::Send { recv: None, args, .. } = *lowered.node else { return None };
    let mut args_with_self = vec![Expr::new(span, ExprNode::SelfRef)];
    args_with_self.extend(args);
    let mut out = send(helper(), MIXES_IN, args_with_self);
    out.span = span;
    Some(out)
}

const MIXES_IN: &str = "mixes_in_class_methods";

/// Whether `expr` is a lowered `mixes_in_class_methods` (run wherever
/// the body is emitted; `project` refuses it off the CRuby trees).
pub(crate) fn is_mixes_in(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Send { recv: Some(recv), method, .. }
        if method.as_str() == MIXES_IN
            && matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == HELPER))
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

/// Keep the carried type aliases a check reads; record every one.
///
/// sorbet-runtime resolves an alias at its first check, so one that only
/// `sig`s read (the tree drops those) never runs its block, and the
/// constants it names need not load. Carrying it anyway made the
/// analysis read those constants, a refusal per name the tree does not
/// carry, at aliases nothing on the tree evaluates.
///
/// A carried alias stays when a lowered check's type names it (by its
/// last segment, so a homonym keeps it too), or when a kept alias names
/// it. Every alias, kept or not, is recorded in
/// `App::sorbet_type_aliases` with its owner: the declared type of a
/// cast that cannot fail (`T.let(nil, T.nilable(Name))`) still reads
/// through it.
pub(crate) fn settle_type_aliases(app: &mut crate::App) {
    // First, so an application's type arguments (which may name these
    // aliases) are gone before the reads below are counted.
    erase_generic_applications(app);
    let mut found: Vec<(crate::ident::ClassId, Symbol, Expr)> = Vec::new();
    for lc in &app.library_classes {
        for (name, value) in &lc.constants {
            if is_alias(value) {
                found.push((lc.name.clone(), name.clone(), value.clone()));
            }
        }
    }
    for model in &app.models {
        for (name, value) in crate::lower::model_to_library::collect_model_constants(model) {
            if is_alias(&value) {
                found.push((model.name.clone(), name, value));
            }
        }
    }
    for tm in &app.test_modules {
        for (name, value) in &tm.constants {
            if is_alias(value) {
                found.push((tm.name.clone(), name.clone(), value.clone()));
            }
        }
    }
    if found.is_empty() {
        return;
    }
    fn const_names(expr: &Expr, out: &mut crate::det::HashSet<Symbol>) {
        if let ExprNode::Const { path } = &*expr.node
            && let Some(last) = path.last()
        {
            out.insert(last.clone());
        }
        expr.node.for_each_child(&mut |child| const_names(child, out));
    }
    fn check_types(expr: &Expr, out: &mut crate::det::HashSet<Symbol>) {
        if is_check(expr)
            && let ExprNode::Send { args, .. } = &*expr.node
            && let Some(ty) = args.get(1)
        {
            const_names(ty, out);
        }
        expr.node.for_each_child(&mut |child| check_types(child, out));
    }
    // A constant path the tree evaluates outside an alias's own block
    // (`attribute :pending, T::Hash[GenericArtifactHandle::HandleNameT,
    // PendingTerms]` in a class body): Ruby resolves it at load, so the
    // alias must be defined there, as sorbet-runtime's `T.type_alias`
    // defines it.
    fn value_paths(expr: &Expr, out: &mut Vec<Vec<Symbol>>) {
        if is_alias(expr) {
            return;
        }
        if let ExprNode::Const { path } = &*expr.node {
            out.push(path.clone());
        }
        expr.node.for_each_child(&mut |child| value_paths(child, out));
    }
    let mut read = crate::det::HashSet::default();
    let mut paths = Vec::new();
    crate::lower::for_each_emit_body_ref(app, &mut |body| {
        check_types(body, &mut read);
        value_paths(body, &mut paths);
    });
    // `Owner::Name` names the alias when `Owner` is its owner's last
    // segment; a bare `Name` by its name, as a check's type does.
    for (owner, name, _) in &found {
        let owner_last = owner.0.as_str().rsplit("::").next().unwrap_or_default();
        if paths.iter().any(|path| match path.as_slice() {
            [.., last] if last != name => false,
            [_] => true,
            [.., parent, _] => parent.as_str() == owner_last,
            [] => false,
        }) {
            read.insert(name.clone());
        }
    }
    loop {
        let before = read.len();
        for (_, name, value) in &found {
            if read.contains(name) {
                const_names(value, &mut read);
            }
        }
        if read.len() == before {
            break;
        }
    }
    let unread = |name: &Symbol, value: &Expr| is_alias(value) && !read.contains(name);
    for lc in &mut app.library_classes {
        lc.constants.retain(|(name, value)| !unread(name, value));
        let kept: crate::det::HashSet<Symbol> = lc.constants.iter().map(|(name, _)| name.clone()).collect();
        lc.top_level_constants.retain(|c| kept.contains(&c.name));
    }
    // A file that held only such aliases (`LooseCurrency = T.type_alias
    // { ... }`) leaves nothing to write.
    app.library_classes.retain(|lc| {
        !(matches!(lc.origin, Some(crate::dialect::LibraryClassOrigin::FileConstants)) && lc.constants.is_empty())
    });
    for tm in &mut app.test_modules {
        tm.constants.retain(|(name, value)| !unread(name, value));
    }
    for model in &mut app.models {
        model.body.retain(|item| match item {
            crate::dialect::ModelBodyItem::Unknown { expr, .. } => match &*expr.node {
                ExprNode::Assign { target: crate::expr::LValue::Const { path }, value } if path.len() == 1 => {
                    !unread(&path[0], value)
                }
                _ => true,
            },
            _ => true,
        });
    }
    app.sorbet_type_aliases = found;
}

/// A generic's type application, `Handle[Rules]`, as sorbet-runtime runs
/// it: the class itself. `T::Generic#[]` is `def [](*types); self; end`
/// (sorbet-runtime 0.6.13502 generic.rb), so
/// `InstructionHandle[QuantityRules].new(NAME)` builds an
/// `InstructionHandle`. The annotations pass drops the `extend
/// T::Generic` that answers `[]` ([`super::library_class::is_sorbet_annotation_mixin`]),
/// and the kept application raised NoMethodError at load (Core's
/// checkouts/one/instructions/merchandise/quantity_rules.rb:16).
///
/// Each `G[...]` whose receiver constant resolves to such a class is
/// written as `G`. Its arguments are type expressions sorbet has already
/// resolved (`T::Boolean`, `T.untyped`, a `type_member` constant), none of
/// which the tree carries as a value.
///
/// Such a class declares `extend T::Generic` in its own body, or
/// inherits from one, and does not define its own class-side `[]` (nor
/// does an ancestor between it and the declaring class). The receiver
/// resolves the way Ruby's lexical lookup reads a written path, from the
/// innermost enclosing namespace of the body outward; a nearer class of
/// the same name shadows a farther generic one.
pub(crate) fn erase_generic_applications(app: &mut crate::App) {
    let declared = generic_declarations(&app.sources);
    if declared.is_empty() {
        return;
    }
    // Every class the app defines, with its parent and whether it
    // answers `[]` itself.
    struct Class {
        parent: Option<String>,
        own_index: bool,
    }
    let mut classes: crate::det::HashMap<String, Class> = crate::det::HashMap::default();
    let class_index = |m: &crate::dialect::MethodDef| {
        m.receiver == crate::dialect::MethodReceiver::Class && m.name.as_str() == "[]"
    };
    let namespace = |name: &str| name.rsplit_once("::").map(|(ns, _)| ns.to_string()).unwrap_or_default();
    let mut parents: Vec<(String, String, String)> = Vec::new();
    for lc in &app.library_classes {
        let name = lc.name.0.as_str().to_string();
        if let Some(parent) = &lc.parent {
            parents.push((name.clone(), parent.0.as_str().to_string(), namespace(&name)));
        }
        let own_index = lc.methods.iter().any(class_index);
        let entry = classes.entry(name).or_insert(Class { parent: None, own_index: false });
        entry.own_index |= own_index;
    }
    for model in &app.models {
        let name = model.name.0.as_str().to_string();
        if let Some(parent) = &model.parent {
            parents.push((name.clone(), parent.0.as_str().to_string(), namespace(&name)));
        }
        let own_index = model.body.iter().any(|item| {
            matches!(item, crate::dialect::ModelBodyItem::Method { method, .. } if class_index(method))
        });
        let entry = classes.entry(name).or_insert(Class { parent: None, own_index: false });
        entry.own_index |= own_index;
    }
    for name in &declared {
        classes.entry(name.clone()).or_insert(Class { parent: None, own_index: false });
    }
    let known: crate::det::HashSet<String> = classes.keys().cloned().collect();
    for (name, parent, scope) in parents {
        let path: Vec<Symbol> = parent.split("::").map(Symbol::from).collect();
        let resolved = resolve_written(&path, &scope, &known);
        if let (Some(resolved), Some(class)) = (resolved, classes.get_mut(&name))
            && resolved != name
        {
            class.parent = Some(resolved);
        }
    }
    // Answers `T::Generic#[]`: no own `[]`, and the declaring class is
    // itself or an ancestor reached without passing one that has its own.
    let applies_self = |name: &str| {
        let mut at = Some(name.to_string());
        let mut seen: Vec<String> = Vec::new();
        while let Some(current) = at.take() {
            if seen.contains(&current) {
                return false;
            }
            let Some(class) = classes.get(&current) else { return false };
            if class.own_index {
                return false;
            }
            if declared.contains(&current) {
                return true;
            }
            at = class.parent.clone();
            seen.push(current);
        }
        false
    };
    let mut generic: crate::det::HashSet<String> = known.iter().filter(|name| applies_self(name)).cloned().collect();
    define_generic_application(app, &declared);
    if generic.is_empty() {
        return;
    }
    // A constant naming a generic (`Optional = ::Adt::Optional`, or one
    // naming such a constant) is that module: `Optional[X]` through it is
    // `T::Generic#[]` on the module, and erased the same way. Settled to
    // a fixed point so a chain of constants resolves.
    let aliases: Vec<(String, Vec<Symbol>, String)> = app
        .library_classes
        .iter()
        .flat_map(|lc| {
            let owner = lc.name.0.as_str().to_string();
            lc.constants.iter().filter_map(move |(name, value)| match &*value.node {
                ExprNode::Const { path } => Some((format!("{owner}::{}", name.as_str()), path.clone(), owner.clone())),
                _ => None,
            })
        })
        .collect();
    let mut known = known;
    loop {
        let mut grew = false;
        for (name, path, scope) in &aliases {
            if generic.contains(name) {
                continue;
            }
            if resolve_written(path, scope, &generic).is_some() {
                known.insert(name.clone());
                generic.insert(name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut rewrite = |owner: Option<&crate::ident::ClassId>, body: &mut Expr| {
        let scope = owner.map(|o| o.0.as_str()).unwrap_or("");
        erase_in(body, scope, &known, &generic);
    };
    crate::lower::for_each_owned_hook_body(app, &mut rewrite);
    for tm in &mut app.test_modules {
        let scope = tm.name.0.as_str().to_string();
        let f = |e: &mut Expr| erase_in(e, &scope, &known, &generic);
        if let Some(setup) = tm.setup.as_mut() {
            f(setup);
        }
        for test in &mut tm.tests {
            f(&mut test.body);
        }
        for (_, value) in &mut tm.constants {
            f(value);
        }
        for method in &mut tm.helpers {
            f(&mut method.body);
            for default in method.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                f(default);
            }
        }
        for class in &mut tm.inner_classes {
            let scope = class.name.0.as_str().to_string();
            let f = |e: &mut Expr| erase_in(e, &scope, &known, &generic);
            for method in &mut class.methods {
                f(&mut method.body);
                for default in method.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                    f(default);
                }
            }
            for (_, value) in &mut class.constants {
                f(value);
            }
            for call in &mut class.unknown_calls {
                f(call);
            }
            for initializer in &mut class.class_ivar_initializers {
                f(initializer);
            }
        }
    }
}

/// The `T::Generic#[]` the dropped `extend T::Generic` provided, defined
/// on each emitted class or module that declared it and has no class-side
/// `[]` of its own: `def self.[](*types); self; end`. The tree's own code
/// no longer applies a generic (above), but a gem that loads real
/// sorbet-runtime still does in its `sig` blocks, against the emitted
/// module (shopify_metafields' `::Shopify::Adt::Result[T.attached_class,
/// ...]`, run when the gem's `Type.coerce` is defined).
fn define_generic_application(app: &mut crate::App, declared: &crate::det::HashSet<String>) {
    for lc in &mut app.library_classes {
        if !declared.contains(lc.name.0.as_str()) {
            continue;
        }
        let has_index = lc
            .methods
            .iter()
            .any(|m| m.receiver == crate::dialect::MethodReceiver::Class && m.name.as_str() == "[]");
        if has_index {
            continue;
        }
        let span = crate::span::Span::synthetic();
        lc.methods.push(crate::dialect::MethodDef {
            name_span: span.clone(),
            name: Symbol::from("[]"),
            receiver: crate::dialect::MethodReceiver::Class,
            visibility: crate::dialect::MethodVisibility::Public,
            params: vec![crate::dialect::Param::rest(Symbol::from("types"))],
            unsupported_formals: None,
            has_anonymous_block: false,
            body: Expr::new(span, ExprNode::SelfRef),
            signature: None,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(lc.name.0.clone()),
            kind: crate::dialect::AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

/// `G[...]` → `G` throughout `expr`, for a `G` that resolves from
/// `scope` to a class in `generic`.
fn erase_in(
    expr: &mut Expr,
    scope: &str,
    known: &crate::det::HashSet<String>,
    generic: &crate::det::HashSet<String>,
) {
    expr.node.for_each_child_mut(&mut |child| erase_in(child, scope, known, generic));
    let ExprNode::Send { recv: Some(recv), method, block: None, .. } = &*expr.node else { return };
    if method.as_str() != "[]" {
        return;
    }
    let ExprNode::Const { path } = &*recv.node else { return };
    if resolve_written(path, scope, known).is_some_and(|name| generic.contains(&name)) {
        let ExprNode::Send { recv: Some(recv), .. } = std::mem::replace(&mut *expr.node, ExprNode::Lit { value: Literal::Nil })
        else {
            unreachable!()
        };
        *expr = recv;
    }
}

/// The class a written constant path names from a body in `scope`: a
/// rooted `::A::B` is the top-level `A::B`; otherwise the innermost
/// `scope` prefix under which the path names a class in `known`.
fn resolve_written(path: &[Symbol], scope: &str, known: &crate::det::HashSet<String>) -> Option<String> {
    let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
    let written = path.iter().skip(usize::from(rooted)).map(|s| s.as_str()).collect::<Vec<_>>().join("::");
    if written.is_empty() {
        return None;
    }
    if rooted {
        return known.contains(&written).then_some(written);
    }
    let segments: Vec<&str> = if scope.is_empty() { Vec::new() } else { scope.split("::").collect() };
    (0..=segments.len()).rev().find_map(|n| {
        let name = if n == 0 { written.clone() } else { format!("{}::{written}", segments[..n].join("::")) };
        known.contains(&name).then_some(name)
    })
}

/// The qualified names of the classes and modules whose own body
/// declares `extend T::Generic` (or `extend(::T::Generic)`, alone or
/// beside other modules), across the app's sources.
fn generic_declarations(sources: &[crate::span::SourceFile]) -> crate::det::HashSet<String> {
    fn extends_generic(stmt: &Node<'_>) -> bool {
        let Some(call) = stmt.as_call_node() else { return false };
        if call.receiver().is_some() || constant_id_str(&call.name()) != "extend" {
            return false;
        }
        call.arguments().is_some_and(|args| {
            args.arguments().iter().any(|arg| {
                super::util::constant_path_segments_strs(&arg).is_some_and(|path| path == ["T", "Generic"])
            })
        })
    }
    fn walk(node: &Node<'_>, scope: &[String], out: &mut crate::det::HashSet<String>) {
        if let Some(program) = node.as_program_node() {
            return walk(&program.statements().as_node(), scope, out);
        }
        if let Some(statements) = node.as_statements_node() {
            for stmt in statements.body().iter() {
                walk(&stmt, scope, out);
            }
            return;
        }
        let (path, body) = if let Some(class) = node.as_class_node() {
            (super::util::class_name_path(&class), class.body())
        } else if let Some(module) = node.as_module_node() {
            (super::util::module_name_path(&module), module.body())
        } else {
            return;
        };
        let Some(path) = path else { return };
        let mut inner = scope.to_vec();
        inner.extend(path);
        let Some(body) = body else { return };
        let statements = super::util::flatten_statements(body);
        if statements.iter().any(extends_generic) {
            out.insert(inner.join("::"));
        }
        for stmt in &statements {
            walk(stmt, &inner, out);
        }
    }
    let mut out = crate::det::HashSet::default();
    for source in sources {
        if !source.text.contains("T::Generic") {
            continue;
        }
        let result = ruby_prism::parse(source.text.as_bytes());
        walk(&result.node(), &[], &mut out);
    }
    out
}
