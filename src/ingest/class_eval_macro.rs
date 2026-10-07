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
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.is_module {
            continue;
        }
        let modules = extended_modules(lc, &app.library_classes);
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
            .filter_map(|inc| resolve_in_ancestry(lc, inc.0.as_str(), &app.library_classes))
            .flat_map(included_hook_calls)
            .map(|call| (call, true));
        for (call, hook) in own.chain(hooked).collect::<Vec<_>>() {
            let ExprNode::Send { method, args, block: None, .. } = &*call.node else { continue };
            let Some(def) = modules.iter().find_map(|m| module_method(m, method, &app.library_classes, &mut Vec::new()))
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
    for (i, lc) in app.library_classes.iter().enumerate() {
        if lc.origin.is_some() {
            continue;
        }
        for (j, inc) in lc.includes.iter().enumerate() {
            let written = inc.0.as_str();
            if written.starts_with("::") || resolve(lc, written, true, &app.library_classes).is_some() {
                continue;
            }
            let Some(found) = resolve_in_ancestry(lc, written, &app.library_classes) else { continue };
            if resolve(lc, found.name.0.as_str(), true, &app.library_classes).is_some_and(|m| m.name == found.name) {
                planned.push((i, j, found.name.clone()));
            }
        }
    }
    for (i, j, name) in planned {
        app.library_classes[i].includes[j] = name;
    }
}

/// `written` as Ruby resolves it from `lc`'s namespace outwards, among
/// the app's library classes (`module: true` for a module).
fn resolve<'a>(lc: &LibraryClass, written: &str, module: bool, classes: &'a [LibraryClass]) -> Option<&'a LibraryClass> {
    let written = written.trim_start_matches("::");
    let scope: Vec<&str> = lc.name.0.as_str().split("::").collect();
    (0..=scope.len()).rev().find_map(|n| {
        let name = if n == 0 { written.to_string() } else { format!("{}::{written}", scope[..n].join("::")) };
        classes.iter().find(|c| c.is_module == module && c.origin.is_none() && c.name.0.as_str() == name)
    })
}

/// `written` as Ruby resolves it in `lc`'s class body: lexically, then
/// as a constant of an app library superclass (`include(WithIdentifiers)`
/// in `Customers::CustomerEntity < CustomerFoundations::Entity` names
/// `CustomerFoundations::Entity::WithIdentifiers`).
fn resolve_in_ancestry<'a>(lc: &LibraryClass, written: &str, classes: &'a [LibraryClass]) -> Option<&'a LibraryClass> {
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
        if let Some(found) = classes.iter().find(|m| m.is_module && m.origin.is_none() && m.name.0.as_str() == name) {
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
fn extended_modules<'a>(lc: &'a LibraryClass, classes: &'a [LibraryClass]) -> Vec<&'a LibraryClass> {
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
    classes: &'a [LibraryClass],
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
