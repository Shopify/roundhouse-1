//! A class macro whose body writes methods with a `class_eval` heredoc,
//! expanded into those methods for the class that calls it.
//!
//! Core's `CustomerFoundations::ComplexStruct::Base` does
//! `extend(ClassMethods)`, and `ClassMethods#attribute(name, type, …)`
//! ends in
//!
//! ```ruby
//! class_eval(<<~RUBY, caller&.path, caller&.lineno)
//!   def #{name}
//!     @attributes[:#{name}]
//!   end
//! RUBY
//! ```
//!
//! Each struct (`CustomerFoundations::Id`) then calls `attribute :value,
//! String`. Nothing in the source defines `Id#value`, so every caller's
//! `id.value` resolved to no method.
//!
//! The expansion substitutes the call's literal Symbol (or String)
//! arguments for the macro's positional parameters in the heredoc. It
//! applies only when every interpolation is one of those parameters, so
//! the result is the exact text Ruby evaluates. That text is parsed as a
//! class body, and its instance `def`s join the calling class. The macro
//! must reach the class the way Ruby reaches it: an instance method of an
//! app module that the class, or an app library ancestor, `extend`s,
//! including modules that module includes. The call is either in the
//! class body or a top-level `base.<macro>(…)` in the `self.included(base)`
//! hook of a module the class includes (Core's `Entity::WithIdentifiers`
//! does `base.attribute(:id, CustomerFoundations::Id)`).
//!
//! In the Ruby tree the macro itself runs: the emit replays the `extend`
//! and the call (`emit::ruby::library`'s `inherited_class_macro_calls`),
//! or the include runs the hook, and writes the class without these copies
//! (`App::class_eval_macro_expansions`). Analysis and the other targets
//! use the copies.

use crate::App;
use crate::app::ClassEvalExpansion;
use crate::dialect::{LibraryClass, MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode, InterpPart, Literal};
use crate::ident::{ClassId, Symbol};

pub fn expand_class_eval_macros(app: &mut App) {
    let mut planned: Vec<(usize, Vec<(ClassEvalExpansion, Vec<MethodDef>)>)> = Vec::new();
    let classes = Classes::indexed(&app.library_classes);
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.is_module {
            continue;
        }
        let modules = extended_modules(lc, &classes);
        if modules.is_empty() {
            continue;
        }
        let mut out = Vec::new();
        let own = lc.unknown_calls.iter().filter_map(|call| {
            let ExprNode::Send { recv, .. } = &*call.node else { return None };
            recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)).then_some((call, false))
        });
        let hooked = lc
            .includes
            .iter()
            .filter_map(|inc| ancestry_resolve(lc, inc.0.as_str(), &classes))
            .flat_map(included_hook_calls)
            .map(|call| (call, true));
        for (call, hook) in own.chain(hooked).collect::<Vec<_>>() {
            let ExprNode::Send { method, args, block: None, .. } = &*call.node else { continue };
            let Some(def) = modules.iter().find_map(|m| module_method(m, method, &classes, &mut Vec::new()))
            else {
                continue;
            };
            let Some(src) = heredoc_source(def, args) else { continue };
            let methods: Vec<MethodDef> = parse_methods(&src)
                .into_iter()
                .filter(|m| m.receiver == MethodReceiver::Instance)
                .filter(|m| !lc.methods.iter().any(|own| own.receiver == MethodReceiver::Instance && own.name == m.name))
                .collect();
            if methods.is_empty() {
                continue;
            }
            let names = methods.iter().map(|m| m.name.clone()).collect();
            out.push((ClassEvalExpansion { call: call.span, methods: names, hook }, methods));
        }
        if !out.is_empty() {
            planned.push((i, out));
        }
    }
    for (i, expansions) in planned {
        let name = app.library_classes[i].name.clone();
        for (expansion, mut methods) in expansions {
            for m in &mut methods {
                m.enclosing_class = name.0.as_str().rsplit("::").next().map(Symbol::from);
            }
            app.library_classes[i].methods.extend(methods);
            app.class_eval_macro_expansions.entry(name.clone()).or_default().push(expansion);
        }
    }
}

/// Qualify an `include M` that Ruby resolves through the class's app
/// superclasses rather than lexically. Constant lookup in a class body
/// tries the lexical scopes, then the ancestors' constants, so
/// `include(WithIdentifiers)` in `Customers::CustomerEntity <
/// CustomerFoundations::Entity` names
/// `CustomerFoundations::Entity::WithIdentifiers`. Ingest records the
/// name as written, which no class is, so the include was refused as
/// unresolved and none of the module's methods or hooks reached the
/// class. The full name replaces the written one only when no lexical
/// scope declares the written name and the full name itself resolves,
/// from the class, to the same module.
pub fn qualify_inherited_includes(app: &mut App) {
    let mut planned: Vec<(usize, usize, ClassId)> = Vec::new();
    let classes = Classes::indexed(&app.library_classes);
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.origin.is_some() {
            continue;
        }
        for (j, inc) in lc.includes.iter().enumerate() {
            let written = inc.0.as_str();
            if written.starts_with("::") || resolve(lc, written, true, &classes).is_some() {
                continue;
            }
            let Some(found) = ancestry_resolve(lc, written, &classes) else { continue };
            if resolve(lc, found.name.0.as_str(), true, &classes).is_some_and(|m| m.name == found.name) {
                planned.push((i, j, found.name.clone()));
            }
        }
    }
    for (i, j, name) in planned {
        app.library_classes[i].includes[j] = name;
    }
}

/// The app's library classes, looked up by name: scanned, or through
/// an index of each (name, is_module)'s first app (origin-less) class.
/// The passes here resolve per namespace prefix, per call, per class,
/// which scanned was quadratic in the library classes.
struct Classes<'a> {
    all: &'a [LibraryClass],
    first: Option<crate::det::HashMap<(&'a str, bool), &'a LibraryClass>>,
}

impl<'a> Classes<'a> {
    fn scanned(all: &'a [LibraryClass]) -> Self {
        Classes { all, first: None }
    }

    fn indexed(all: &'a [LibraryClass]) -> Self {
        let mut first: crate::det::HashMap<(&str, bool), &LibraryClass> = crate::det::HashMap::default();
        for c in all.iter().filter(|c| c.origin.is_none()) {
            first.entry((c.name.0.as_str(), c.is_module)).or_insert(c);
        }
        Classes { all, first: Some(first) }
    }

    /// The first app class (`module: true`, module) named `name`.
    fn find(&self, name: &str, module: bool) -> Option<&'a LibraryClass> {
        match &self.first {
            Some(first) => first.get(&(name, module)).copied(),
            None => self.all.iter().find(|c| c.is_module == module && c.origin.is_none() && c.name.0.as_str() == name),
        }
    }
}

/// `written` as Ruby resolves it from `lc`'s namespace outwards, among
/// the app's library classes (`module: true` for a module).
fn resolve<'a>(lc: &LibraryClass, written: &str, module: bool, classes: &Classes<'a>) -> Option<&'a LibraryClass> {
    let written = written.trim_start_matches("::");
    let scope: Vec<&str> = lc.name.0.as_str().split("::").collect();
    (0..=scope.len()).rev().find_map(|n| {
        let name = if n == 0 { written.to_string() } else { format!("{}::{written}", scope[..n].join("::")) };
        classes.find(&name, module)
    })
}

/// `written` as Ruby resolves it in `lc`'s class body: lexically, then
/// as a constant of an app library superclass (`include(WithIdentifiers)`
/// in `Customers::CustomerEntity < CustomerFoundations::Entity` names
/// `CustomerFoundations::Entity::WithIdentifiers`).
pub(super) fn resolve_in_ancestry<'a>(lc: &LibraryClass, written: &str, classes: &'a [LibraryClass]) -> Option<&'a LibraryClass> {
    ancestry_resolve(lc, written, &Classes::scanned(classes))
}

fn ancestry_resolve<'a>(lc: &LibraryClass, written: &str, classes: &Classes<'a>) -> Option<&'a LibraryClass> {
    if let Some(found) = resolve(lc, written, true, classes) {
        return Some(found);
    }
    let written = written.trim_start_matches("::");
    let mut seen: Vec<&ClassId> = vec![&lc.name];
    let mut at = lc.parent.as_ref().and_then(|p| resolve(lc, p.0.as_str(), false, classes));
    while let Some(c) = at.take() {
        if seen.contains(&&c.name) {
            break;
        }
        seen.push(&c.name);
        let name = format!("{}::{written}", c.name.0.as_str());
        if let Some(found) = classes.find(&name, true) {
            return Some(found);
        }
        at = c.parent.as_ref().and_then(|p| resolve(c, p.0.as_str(), false, classes));
    }
    None
}

/// The top-level `base.<name>(…)` statements of module `m`'s
/// `self.included(base)` hook: calls Ruby makes on the including class.
fn included_hook_calls(m: &LibraryClass) -> Vec<&Expr> {
    let Some(hook) = m.methods.iter().find(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == "included")
    else {
        return Vec::new();
    };
    let [base] = hook.params.as_slice() else { return Vec::new() };
    if base.keyword || base.rest || base.forwarding || base.default.is_some() {
        return Vec::new();
    }
    let statements: Vec<&Expr> = match &*hook.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&hook.body],
    };
    statements
        .into_iter()
        .filter(|s| {
            matches!(&*s.node, ExprNode::Send { recv: Some(r), .. }
                if matches!(&*r.node, ExprNode::Var { name, .. } if *name == base.name))
        })
        .collect()
}

/// The app modules a bare `extend M` in `lc` or in one of its app library
/// superclasses gives it as singleton methods.
fn extended_modules<'a>(lc: &'a LibraryClass, classes: &Classes<'a>) -> Vec<&'a LibraryClass> {
    let mut out = Vec::new();
    let mut at = Some(lc);
    let mut seen: Vec<&ClassId> = Vec::new();
    while let Some(c) = at.take() {
        if seen.contains(&&c.name) {
            break;
        }
        seen.push(&c.name);
        for call in &c.unknown_calls {
            let ExprNode::Send { recv: None, method, args, .. } = &*call.node else { continue };
            if method.as_str() != "extend" {
                continue;
            }
            for arg in args {
                if let ExprNode::Const { path } = &*arg.node {
                    let written = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
                    out.extend(resolve(c, &written, true, classes));
                }
            }
        }
        at = c.parent.as_ref().and_then(|p| resolve(c, p.0.as_str(), false, classes));
    }
    out
}

/// The instance method `name` of module `m` or of an app module it
/// includes.
fn module_method<'a>(
    m: &'a LibraryClass,
    name: &Symbol,
    classes: &Classes<'a>,
    seen: &mut Vec<&'a ClassId>,
) -> Option<&'a MethodDef> {
    if seen.contains(&&m.name) {
        return None;
    }
    seen.push(&m.name);
    m.methods
        .iter()
        .find(|d| d.receiver == MethodReceiver::Instance && d.name == *name)
        .or_else(|| {
            m.includes.iter().find_map(|inc| {
                resolve(m, inc.0.as_str(), true, classes).and_then(|inner| module_method(inner, name, classes, seen))
            })
        })
}

/// The heredoc `def`'s `class_eval` evaluates for this call, with each
/// interpolated positional parameter replaced by the call's literal
/// argument. `None` when the macro has no such `class_eval`, or when any
/// interpolation is not a parameter that a literal Symbol or String fills.
fn heredoc_source(def: &MethodDef, args: &[Expr]) -> Option<String> {
    let positional: Vec<&Symbol> = def
        .params
        .iter()
        .filter(|p| !p.keyword && !p.rest && !p.forwarding && !p.from_keyword && !p.from_kwrest)
        .map(|p| &p.name)
        .collect();
    let literal = |param: &Symbol| -> Option<String> {
        let at = positional.iter().position(|p| *p == param)?;
        match &*args.get(at)?.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
            ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
            _ => None,
        }
    };
    let mut found: Option<Option<String>> = None;
    find_class_eval(&def.body, &mut |parts| {
        if found.is_some() {
            return;
        }
        let mut src = String::new();
        let mut ok = true;
        for part in parts {
            match part {
                InterpPart::Text { value } => src.push_str(value),
                InterpPart::Expr { expr } => match &*expr.node {
                    ExprNode::Var { name, .. } => match literal(name) {
                        Some(text) => src.push_str(&text),
                        None => ok = false,
                    },
                    _ => ok = false,
                },
            }
        }
        found = Some(ok.then_some(src));
    });
    found.flatten().filter(|src| src.contains("def "))
}

fn find_class_eval(expr: &Expr, f: &mut dyn FnMut(&[InterpPart])) {
    if let ExprNode::Send { recv, method, args, .. } = &*expr.node
        && matches!(method.as_str(), "class_eval" | "module_eval")
        && recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        && let Some(first) = args.first()
        && let ExprNode::StringInterp { parts } = &*first.node
    {
        f(parts);
        return;
    }
    expr.node.for_each_child(&mut |c| find_class_eval(c, f));
}

/// The methods of `body` read as a class body, in its own parse scope
/// (the same isolation `ingest::delegate` uses for its forwarders).
fn parse_methods(body: &str) -> Vec<MethodDef> {
    let src = format!("class RoundhouseClassEvalMacro\n{body}\nend\n");
    let (parsed, diags) =
        crate::ingest::prism::scope(|| crate::ingest::ingest_library_classes(src.as_bytes(), "<class_eval>"));
    match parsed {
        Ok(classes) if diags.is_empty() => classes.into_iter().flat_map(|c| c.methods).collect(),
        _ => Vec::new(),
    }
}

/// A class-body loop over a literal list whose block is one `class_eval`
/// of a string, unrolled into the methods Ruby evaluates. activemerchant
/// 1.137.0's `CreditCard`:
///
/// ```ruby
/// %w(month year start_month start_year).each do |m|
///   class_eval <<~RUBY, __FILE__, __LINE__ + 1
///     def #{m}=(v)
///       @#{m} = case v ... end
///     end
///   RUBY
/// end
/// ```
///
/// The walk captured no call on a literal receiver, so the loop and its
/// four writers were dropped with no diagnostic (`Model#initialize`'s
/// `send("month=", 9)` then raised NoMethodError). Each element (a
/// literal Symbol or String) replaces the block parameter in the string,
/// so the result is the exact text Ruby evaluates.
///
/// The loop may also run over a literal Hash, its block taking the key
/// and the value, and its block may be one such loop over a value the
/// outer block bound. activeresource's `HttpMock` writes its verbs so:
///
/// ```ruby
/// { true  => %w[post patch put],
///   false => %w[get delete head] }.each do |has_body, methods|
///   methods.each do |method|
///     module_eval <<-EOE, __FILE__, __LINE__ + 1
///       def #{method}(path, #{'body, ' if has_body}headers, options = {})
///         request = ActiveResource::Request.new(:#{method}, path, #{has_body ? 'body, ' : 'nil, '}headers, options)
///         process(request)
///       end
///     EOE
///   end
/// end
/// ```
///
/// An interpolation is a bound name, or a conditional on one whose
/// branches are literal strings or nil, written as Ruby writes it
/// (`nil` and `false` interpolate as "" and "false"). This applies
/// only when every interpolation is one of those. The text is read as a
/// class body. Its methods are public, as a string `class_eval` starts
/// public. The file and line arguments only locate the text. `None` for
/// any other shape, and the caller keeps the call.
pub(super) fn unrolled_class_eval_loop(stmt: &Expr) -> Option<Vec<MethodDef>> {
    let mut out = Vec::new();
    unroll(stmt, &mut Vec::new(), &mut out)?;
    Some(out)
}

/// A value the unrolled loop binds.
#[derive(Clone)]
enum Bound {
    Text(String),
    Bool(bool),
    Nil,
    List(Vec<Bound>),
}

impl Bound {
    fn literal(expr: &Expr) -> Option<Bound> {
        Some(match &*expr.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Bound::Text(value.as_str().to_string()),
            ExprNode::Lit { value: Literal::Str { value } } => Bound::Text(value.clone()),
            ExprNode::Lit { value: Literal::Bool { value } } => Bound::Bool(*value),
            ExprNode::Lit { value: Literal::Nil } => Bound::Nil,
            ExprNode::Array { elements, .. } => Bound::List(elements.iter().map(Bound::literal).collect::<Option<_>>()?),
            _ => return None,
        })
    }

    /// The text `"#{value}"` writes.
    fn text(&self) -> Option<String> {
        match self {
            Bound::Text(s) => Some(s.clone()),
            Bound::Bool(b) => Some(b.to_string()),
            Bound::Nil => Some(String::new()),
            Bound::List(_) => None,
        }
    }

    fn truthy(&self) -> bool {
        !matches!(self, Bound::Nil | Bound::Bool(false))
    }
}

fn bound<'a>(env: &'a [(Symbol, Bound)], expr: &Expr) -> Option<&'a Bound> {
    let ExprNode::Var { name, .. } = &*expr.node else { return None };
    env.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
}

/// The value of an interpolated expression under `env`.
fn evaluate(env: &[(Symbol, Bound)], expr: &Expr) -> Option<Bound> {
    match &*expr.node {
        ExprNode::Var { .. } => bound(env, expr).cloned(),
        ExprNode::If { cond, then_branch, else_branch } => {
            let branch = if bound(env, cond)?.truthy() { then_branch } else { else_branch };
            evaluate(env, branch)
        }
        ExprNode::Seq { exprs } if exprs.is_empty() => Some(Bound::Nil),
        _ => Bound::literal(expr).filter(|v| !matches!(v, Bound::List(_))),
    }
}

fn unroll(stmt: &Expr, env: &mut Vec<(Symbol, Bound)>, out: &mut Vec<MethodDef>) -> Option<()> {
    let ExprNode::Send { recv: Some(recv), method, args, block: Some(block), .. } = &*stmt.node else {
        return None;
    };
    if method.as_str() != "each" || !args.is_empty() {
        return None;
    }
    let ExprNode::Lambda { params, rest_param: None, block_param: None, body, .. } = &*block.node else {
        return None;
    };
    // Each iteration's bindings for the block parameters.
    let rows: Vec<Vec<Bound>> = match &*recv.node {
        ExprNode::Hash { entries, kwargs: false } => {
            let [_, _] = params.as_slice() else { return None };
            entries
                .iter()
                .map(|(k, v)| Some(vec![Bound::literal(k)?, Bound::literal(v)?]))
                .collect::<Option<_>>()?
        }
        _ => {
            let [_] = params.as_slice() else { return None };
            let items = match &*recv.node {
                ExprNode::Array { .. } => Bound::literal(recv)?,
                _ => bound(env, recv)?.clone(),
            };
            let Bound::List(items) = items else { return None };
            items.into_iter().map(|item| vec![item]).collect()
        }
    };
    let statement = match &*body.node {
        ExprNode::Seq { exprs } => match exprs.as_slice() {
            [only] => only,
            _ => return None,
        },
        _ => body,
    };
    for row in rows {
        let depth = env.len();
        env.extend(params.iter().cloned().zip(row));
        if matches!(&*statement.node, ExprNode::Send { method, block: Some(_), .. } if method.as_str() == "each") {
            unroll(statement, env, out)?;
        } else {
            out.extend(evaluated_class_eval(statement, env)?);
        }
        env.truncate(depth);
    }
    Some(())
}

/// The methods one string `class_eval` writes under `env`.
fn evaluated_class_eval(statement: &Expr, env: &[(Symbol, Bound)]) -> Option<Vec<MethodDef>> {
    let ExprNode::Send { recv: eval_recv, method: eval, args: eval_args, block: None, .. } = &*statement.node else {
        return None;
    };
    if !matches!(eval.as_str(), "class_eval" | "module_eval")
        || eval_recv.as_ref().is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef))
        || eval_args.is_empty()
        || eval_args.len() > 3
    {
        return None;
    }
    let ExprNode::StringInterp { parts } = &*eval_args[0].node else { return None };
    let mut src = String::new();
    for part in parts {
        match part {
            InterpPart::Text { value } => src.push_str(value),
            InterpPart::Expr { expr } => src.push_str(&evaluate(env, expr)?.text()?),
        }
    }
    if !src.contains("def ") {
        return None;
    }
    let methods = parse_methods(&src);
    if methods.is_empty() {
        return None;
    }
    Some(
        methods
            .into_iter()
            .map(|mut m| {
                m.visibility = crate::dialect::MethodVisibility::Public;
                m.name_span = crate::span::Span::synthetic();
                m
            })
            .collect(),
    )
}
