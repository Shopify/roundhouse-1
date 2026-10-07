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
//! including modules that module includes.
//!
//! In the Ruby tree the macro itself runs: the emit replays the `extend`
//! and the call (`emit::ruby::library`'s `inherited_class_macro_calls`),
//! and writes the class without these copies
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
        for call in &lc.unknown_calls {
            let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { continue };
            if !recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef)) {
                continue;
            }
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
            out.push((ClassEvalExpansion { call: call.span, methods: names }, methods));
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
