//! The columns of a model whose type an app macro can replace at load.
//!
//! Rails' `attribute name, type` (attribute_registration.rb) replaces a
//! column's cast type: reads, writes, hydration and binds then go through
//! the declared type object. A literal `attribute :col, :type` in the body
//! is applied statically (`markers::attribute_api_decls`). This module is
//! about the other case, a class-body call of an app class macro that
//! reaches `attribute` when it runs, as Core's
//! `model_uuid_attributes :id, :shop_id` does through
//! `register_uuid_attribute`. Which columns it retypes, and with which
//! type, is decided when the macro runs (Core's allowlist, its boot
//! configuration), so it is not decided here.
//!
//! What is decided here is which columns get the compiled path that
//! consults a declared type: the literal Symbol or String arguments of
//! such a call (in Array literals too) that name a column of the model's
//! table. The runtime's `Base.attribute` raises for any other name, so a
//! macro that retypes a column passed some other way fails loudly rather
//! than being ignored. The analyzer types these columns gradually and
//! reports `attribute_type_unevaluated` (`analyze::model_attributes`).
//!
//! The walk follows what runs at class-body time: the macro's body, the
//! class methods it calls on `self`, and the blocks of the calls it makes,
//! except blocks that run later or on another object (`define_method`,
//! `lambda`, `proc`, `Module.new`, `Class.new`). The class side is the
//! model's own class methods, its model ancestors' (concern
//! `class_methods` are spliced into them), and the instance methods of the
//! app modules its body or an ancestor's `extend`s.

use crate::App;
use crate::dialect::{LibraryClass, MethodDef, MethodReceiver, Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

/// Calls whose block does not run in the class body.
const LATER_BLOCKS: &[&str] = &["define_method", "define_singleton_method", "lambda", "proc", "new", "instance_exec"];

/// The model's retypable columns, in table order: those its own body's
/// calls name, and its model ancestors' (an STI subclass shares the
/// table, and Rails applies a superclass's declarations to it).
pub(crate) fn retypable_attributes(model: &Model, app: &App) -> Vec<Symbol> {
    let Some(table) = app.schema.tables.get(&model.table.0) else { return Vec::new() };
    let mut names: Vec<String> = Vec::new();
    let mut at = Some(model);
    let mut visited: Vec<&str> = Vec::new();
    while let Some(m) = at {
        if visited.contains(&m.name.0.as_str()) {
            break;
        }
        visited.push(m.name.0.as_str());
        for (_, call_names) in retyping_calls(m, app) {
            names.extend(call_names);
        }
        at = m.parent.as_ref().and_then(|p| app.models.iter().find(|x| &x.name == p));
    }
    table.columns.iter().filter(|c| names.iter().any(|n| n == c.name.as_str())).map(|c| c.name.clone()).collect()
}

/// `attribute_type_unevaluated` for each column a body call can retype,
/// at the call.
pub(crate) fn diagnose(app: &App) -> Vec<crate::diagnostic::Diagnostic> {
    use crate::diagnostic::{Diagnostic, DiagnosticKind};
    let mut out = Vec::new();
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else { continue };
        for (call, names) in retyping_calls(model, app) {
            let ExprNode::Send { method, .. } = &*call.node else { continue };
            for col in table.columns.iter().filter(|c| names.iter().any(|n| n == c.name.as_str())) {
                let kind = DiagnosticKind::AttributeTypeUnevaluated {
                    model: Symbol::from(model.name.0.as_str()),
                    attribute: col.name.clone(),
                    call: method.clone(),
                };
                let mut d = Diagnostic {
                    span: call.span,
                    severity: Diagnostic::default_severity(&kind),
                    kind,
                    message: String::new(),
                };
                d.message = Diagnostic::stub_text(&d.kind);
                out.push(d);
            }
        }
    }
    out
}

/// `composite_primary_key` for each model whose table has a composite
/// primary key and which declares no `self.primary_key`, itself or
/// through a model ancestor. The compiled record keys on one column
/// (`adapter_emit::key_column`); Rails keys such a model on all of them.
pub(crate) fn diagnose_composite_primary_keys(app: &App) -> Vec<crate::diagnostic::Diagnostic> {
    use crate::diagnostic::{Diagnostic, DiagnosticKind};
    let mut out = Vec::new();
    for model in &app.models {
        let Some(table) = app.schema.tables.get(&model.table.0) else { continue };
        if table.primary_key_columns.is_empty() {
            continue;
        }
        let mut declared = false;
        let mut at = Some(model);
        let mut visited: Vec<&str> = Vec::new();
        while let Some(m) = at {
            if declared || visited.contains(&m.name.0.as_str()) {
                break;
            }
            visited.push(m.name.0.as_str());
            declared = m.primary_key.is_some();
            at = m.parent.as_ref().and_then(|p| app.models.iter().find(|x| &x.name == p));
        }
        if declared {
            continue;
        }
        let kind = DiagnosticKind::CompositePrimaryKey {
            model: Symbol::from(model.name.0.as_str()),
            table: table.name.clone(),
            columns: table.primary_key_columns.clone(),
        };
        let mut d = Diagnostic { span: model.span, severity: Diagnostic::default_severity(&kind), kind, message: String::new() };
        d.message = Diagnostic::stub_text(&d.kind);
        out.push(d);
    }
    out
}

/// The model's own body calls of an app class macro that reaches
/// `attribute`, each with the literal names it passes.
fn retyping_calls<'a>(model: &'a Model, app: &'a App) -> Vec<(&'a Expr, Vec<String>)> {
    let side = ClassSide::of(model, app);
    let mut out = Vec::new();
    for call in body_statements(model) {
        let ExprNode::Send { recv, method, args, .. } = &*call.node else { continue };
        if !on_self(recv) || method.as_str() == "attribute" {
            continue;
        }
        let Some(def) = side.class_method(method.as_str()) else { continue };
        let mut seen = vec![method.as_str().to_string()];
        if side.reaches_attribute(&def.body, &mut seen) {
            let mut names = Vec::new();
            for arg in args {
                literal_names(arg, &mut names);
            }
            out.push((call, names));
        }
    }
    out
}

fn on_self(recv: &Option<Expr>) -> bool {
    recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
}

fn body_statements(model: &Model) -> impl Iterator<Item = &Expr> {
    model.body.iter().filter_map(|item| match item {
        ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. } => Some(expr),
        _ => None,
    })
}

fn literal_names(arg: &Expr, out: &mut Vec<String>) {
    match &*arg.node {
        ExprNode::Lit { value: Literal::Sym { value } } => out.push(value.as_str().to_string()),
        ExprNode::Lit { value: Literal::Str { value } } => out.push(value.clone()),
        ExprNode::Array { elements, .. } => elements.iter().for_each(|e| literal_names(e, out)),
        ExprNode::Splat { value } => literal_names(value, out),
        _ => {}
    }
}

struct ClassSide<'a> {
    app: &'a App,
    own: Vec<&'a MethodDef>,
    chain: Vec<&'a Model>,
    extended: Vec<&'a LibraryClass>,
}

impl<'a> ClassSide<'a> {
    fn of(model: &'a Model, app: &'a App) -> Self {
        let own = model.methods().filter(|d| d.receiver == MethodReceiver::Class).collect();
        let mut chain: Vec<&Model> = Vec::new();
        let mut next = model.parent.as_ref();
        while let Some(name) = next {
            match app.models.iter().find(|m| &m.name == name) {
                Some(m) if !chain.iter().any(|seen| seen.name == m.name) => {
                    chain.push(m);
                    next = m.parent.as_ref();
                }
                _ => break,
            }
        }
        let mut extended = Vec::new();
        for m in std::iter::once(model).chain(chain.iter().copied()) {
            for call in body_statements(m) {
                let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { continue };
                if method.as_str() != "extend" || !on_self(recv) {
                    continue;
                }
                extended.extend(args.iter().filter_map(|a| module_named(app, m.name.0.as_str(), a)));
            }
        }
        Self { app, own, chain, extended }
    }

    fn class_method(&self, name: &str) -> Option<&'a MethodDef> {
        self.own
            .iter()
            .rev()
            .find(|d| d.name.as_str() == name)
            .copied()
            .or_else(|| {
                self.chain.iter().find_map(|m| {
                    m.methods().filter(|d| d.receiver == MethodReceiver::Class && d.name.as_str() == name).last()
                })
            })
            .or_else(|| self.extended.iter().find_map(|m| self.module_method(m, name, &mut Vec::new())))
    }

    fn module_method(&self, m: &'a LibraryClass, name: &str, seen: &mut Vec<String>) -> Option<&'a MethodDef> {
        if seen.iter().any(|s| s == m.name.0.as_str()) {
            return None;
        }
        seen.push(m.name.0.as_str().to_string());
        m.methods.iter().filter(|d| d.receiver == MethodReceiver::Instance && d.name.as_str() == name).last().or_else(|| {
            m.includes.iter().find_map(|inc| {
                let inc = inc.0.as_str().trim_start_matches("::");
                let included =
                    self.app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == inc)?;
                self.module_method(included, name, seen)
            })
        })
    }

    /// Does running `expr` in the class body call `attribute` on the class?
    fn reaches_attribute(&self, expr: &Expr, seen: &mut Vec<String>) -> bool {
        if let ExprNode::Send { recv, method, args, block, .. } = &*expr.node {
            let name = method.as_str();
            if on_self(recv) {
                if name == "attribute" {
                    return true;
                }
                if !seen.iter().any(|s| s == name) {
                    if let Some(def) = self.class_method(name) {
                        seen.push(name.to_string());
                        if self.reaches_attribute(&def.body, seen) {
                            return true;
                        }
                    }
                }
            }
            let later = LATER_BLOCKS.contains(&name);
            let block_runs = |b: &Expr, seen: &mut Vec<String>| match &*b.node {
                ExprNode::Lambda { body, .. } => self.reaches_attribute(body, seen),
                _ => self.reaches_attribute(b, seen),
            };
            return recv.iter().chain(args.iter()).any(|e| self.reaches_attribute(e, seen))
                || (!later && block.iter().any(|b| block_runs(b, seen)));
        }
        // A lambda literal's body runs when it is called, not here.
        if matches!(&*expr.node, ExprNode::Lambda { .. }) {
            return false;
        }
        let mut found = false;
        expr.node.for_each_child(&mut |child| {
            if !found && self.reaches_attribute(child, seen) {
                found = true;
            }
        });
        found
    }
}

/// The app module (not a gem's) a constant names, resolved from `owner`'s
/// namespace outwards.
fn module_named<'a>(app: &'a App, owner: &str, constant: &Expr) -> Option<&'a LibraryClass> {
    let ExprNode::Const { path } = &*constant.node else { return None };
    let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
    let written = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
    let find = |name: &str| app.library_classes.iter().find(|c| c.is_module && c.origin.is_none() && c.name.0.as_str() == name);
    if rooted {
        return find(&written);
    }
    let scope: Vec<&str> = owner.split("::").collect();
    (0..scope.len()).rev().find_map(|n| {
        if n == 0 { find(&written) } else { find(&format!("{}::{written}", scope[..n].join("::"))) }
    })
}
