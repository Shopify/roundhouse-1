//! `record.association(:name)` calls the emitted association object
//! cannot answer as Rails does, refused where they are written.
//!
//! Each model answers `association(name)` for the associations it has
//! (declared, spliced from a concern, inherited from an abstract base or
//! an STI parent; `emit::ruby::library::apply_association_reader`) and
//! raises AssociationNotFoundError for any other name, which Rails does
//! too. Rails has associations the IR does not list, though: the
//! `<name>_attachment` / `<name>_blob` pair `has_one_attached` declares,
//! `has_many_attached`'s plural pair, `has_rich_text`'s `rich_text_<name>`.
//! A program asking for one of those checked clean and raised where
//! Rails answers. So a statically named `association(:x)` on a model
//! whose family (the model, its model ancestors and descendants) has no
//! association `x` is refused here.
//!
//! The same for the parts of the object that are ported only for some
//! kinds (runtime/spinel/active_record_associations.rb): `stale_target?`
//! on a polymorphic belongs_to or a `:through` association. (`scope` is
//! ported for a scoped, `as:` or `:through` association; the runtime
//! names the one whose query it cannot build.) And for
//! `record.<has_many>.reset` continuing into another call: Rails resets
//! the CollectionProxy and answers it, the emitted call answers the
//! association object.

use crate::det::{HashMap, HashSet};
use crate::diagnostic::Diagnostic;
use crate::dialect::{Association, MethodReceiver, Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;
use crate::App;

struct Surface<'a> {
    app: &'a App,
    sole_includer: HashMap<ClassId, ClassId>,
    has_many: HashMap<ClassId, HashSet<Symbol>>,
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    if !calls_association(app) && !resets_collection(app) {
        return Vec::new();
    }
    let surface = Surface {
        app,
        sole_includer: app.sole_includer_of_modules(),
        has_many: crate::lower::assoc_loaded::has_many_by_model(app),
    };
    let mut out = Vec::new();
    // Bodies whose implicit self is a model: the model's own methods and
    // a concern module's (resolved to its sole model includer).
    for model in &app.models {
        for item in &model.body {
            if let ModelBodyItem::Method { method, .. } = item {
                surface.walk(&method.body, Some(&model.name), &mut out);
            }
        }
    }
    for lc in &app.library_classes {
        for m in &lc.methods {
            surface.walk(&m.body, Some(&lc.name), &mut out);
        }
    }
    // Everything else, for explicit receivers.
    let mut rest = |e: &Expr| surface.walk(e, None, &mut out);
    for c in &app.controllers {
        for a in c.actions() {
            rest(&a.body);
        }
    }
    for view in &app.views {
        rest(&view.body);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut rest);
    // A concern method spliced into its model is walked twice.
    let mut seen: Vec<(crate::span::Span, String)> = Vec::new();
    out.retain(|d| {
        let key = (d.span, d.message.clone());
        let fresh = !seen.contains(&key);
        if fresh {
            seen.push(key);
        }
        fresh
    });
    out
}

fn calls_association(app: &App) -> bool {
    fn walk(e: &Expr, found: &mut bool) {
        if *found {
            return;
        }
        if let ExprNode::Send { method, args, .. } = &*e.node {
            if method.as_str() == "association" && args.len() == 1 {
                *found = true;
                return;
            }
        }
        e.node.for_each_child(&mut |c| walk(c, found));
    }
    let mut found = false;
    crate::lower::for_each_hook_body_ref(app, &mut |e| walk(e, &mut found));
    for view in &app.views {
        walk(&view.body, &mut found);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |e| walk(e, &mut found));
    found
}

fn resets_collection(app: &App) -> bool {
    fn walk(e: &Expr, found: &mut bool) {
        if *found {
            return;
        }
        if let ExprNode::Send { recv: Some(r), .. } = &*e.node {
            if matches!(&*r.node, ExprNode::Send { method, args, .. } if method.as_str() == "reset" && args.is_empty()) {
                *found = true;
                return;
            }
        }
        e.node.for_each_child(&mut |c| walk(c, found));
    }
    let mut found = false;
    crate::lower::for_each_hook_body_ref(app, &mut |e| walk(e, &mut found));
    for view in &app.views {
        walk(&view.body, &mut found);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut |e| walk(e, &mut found));
    found
}

fn sym_or_str(e: &Expr) -> Option<Symbol> {
    match &*e.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
        ExprNode::Lit { value: Literal::Str { value } } => Some(Symbol::from(value.as_str())),
        _ => None,
    }
}

impl Surface<'_> {
    fn model(&self, id: &ClassId) -> Option<&Model> {
        self.app.models.iter().find(|m| &m.name == id)
    }

    /// The model the receiver is: a model-typed explicit receiver (every
    /// class of a union a model), or implicit self in a model body or a
    /// concern with one model includer. Several for a union.
    fn owners(&self, recv: Option<&Expr>, enclosing: Option<&ClassId>) -> Vec<ClassId> {
        let implicit = recv.is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
        if implicit {
            let Some(enclosing) = enclosing else { return Vec::new() };
            if self.model(enclosing).is_some() {
                return vec![enclosing.clone()];
            }
            return match self.sole_includer.get(enclosing) {
                Some(m) if self.model(m).is_some() => vec![m.clone()],
                _ => Vec::new(),
            };
        }
        let ids: Vec<ClassId> = match recv.and_then(|r| r.ty.as_ref()) {
            Some(Ty::Class { id, .. }) => vec![id.clone()],
            Some(Ty::Union { variants }) => variants
                .iter()
                .filter(|v| !matches!(v, Ty::Nil))
                .map(|v| match v {
                    Ty::Class { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if ids.iter().all(|id| self.model(id).is_some()) { ids } else { Vec::new() }
    }

    /// The model, its model ancestors and its model descendants: every
    /// class a receiver typed as the model may be at run time, or whose
    /// associations it inherits.
    fn family(&self, id: &ClassId) -> Vec<&Model> {
        let mut out: Vec<&Model> = Vec::new();
        let mut cur = self.model(id);
        while let Some(m) = cur {
            if out.iter().any(|o| o.name == m.name) {
                break;
            }
            out.push(m);
            cur = m.parent.as_ref().and_then(|p| self.model(p));
        }
        let mut grew = true;
        while grew {
            grew = false;
            for m in &self.app.models {
                let under = m.parent.as_ref().is_some_and(|p| out.iter().any(|o| &o.name == p));
                if under && !out.iter().any(|o| o.name == m.name) {
                    out.push(m);
                    grew = true;
                }
            }
        }
        out
    }

    /// The associations named `name` across the family, or None when a
    /// family member answers `association` itself.
    fn lookup(&self, id: &ClassId, name: &Symbol) -> Option<Vec<&Association>> {
        let family = self.family(id);
        let own_dispatch = family.iter().any(|m| {
            m.body.iter().any(|item| {
                matches!(item, ModelBodyItem::Method { method, .. }
                    if method.name.as_str() == "association" && method.receiver == MethodReceiver::Instance)
            })
        });
        if own_dispatch {
            return None;
        }
        Some(family.iter().flat_map(|m| m.associations()).filter(|a| a.name() == name).collect())
    }

    fn walk(&self, e: &Expr, enclosing: Option<&ClassId>, out: &mut Vec<Diagnostic>) {
        // `assert_raises(ActiveRecord::AssociationNotFoundError) { … }`:
        // the authored test states the name is not an association, and
        // the emitted record raises as Rails does.
        if let ExprNode::Send { recv: None, method, args, .. } = &*e.node {
            let expects_not_found = method.as_str() == "assert_raises"
                && args.iter().any(|a| {
                    matches!(&*a.node, ExprNode::Const { path }
                        if path.last().is_some_and(|l| l.as_str() == "AssociationNotFoundError"))
                });
            if expects_not_found {
                return;
            }
        }
        self.visit(e, enclosing, out);
        e.node.for_each_child(&mut |c| self.walk(c, enclosing, out));
    }

    fn visit(&self, e: &Expr, enclosing: Option<&ClassId>, out: &mut Vec<Diagnostic>) {
        let ExprNode::Send { recv, method, args, .. } = &*e.node else { return };
        // Explicit receivers are checked in every body, implicit self
        // only where the enclosing class is known.
        let implicit = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
        if implicit && enclosing.is_none() {
            return;
        }
        if method.as_str() == "association" && args.len() == 1 {
            let Some(name) = sym_or_str(&args[0]) else { return };
            for owner in self.owners(recv.as_ref(), enclosing) {
                if let Some(found) = self.lookup(&owner, &name) {
                    if found.is_empty() {
                        out.push(Diagnostic::unsupported(
                            e.span,
                            None,
                            "association_name",
                            format!(
                                "`association(:{})` on {}: no association `{}` on the model, its ancestors or its subclasses (an attachment's or rich text's association is not modelled), so the emitted record raises AssociationNotFoundError where Rails answers",
                                name.as_str(),
                                owner.0.as_str(),
                                name.as_str()
                            ),
                        ));
                    }
                }
            }
            return;
        }
        // `association(:x).scope` / `.stale_target?` on a kind the
        // runtime does not port.
        if args.is_empty() && matches!(method.as_str(), "scope" | "stale_target?") {
            let Some(r) = recv else { return };
            let ExprNode::Send { recv: owner_recv, method: am, args: aargs, .. } = &*r.node else { return };
            if am.as_str() != "association" || aargs.len() != 1 {
                return;
            }
            let implicit = owner_recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
            if implicit && enclosing.is_none() {
                return;
            }
            let Some(name) = sym_or_str(&aargs[0]) else { return };
            for owner in self.owners(owner_recv.as_ref(), enclosing) {
                let Some(found) = self.lookup(&owner, &name) else { continue };
                let why = found.iter().find_map(|a| unported(a, method.as_str()));
                if let Some(why) = why {
                    out.push(Diagnostic::unsupported(
                        e.span,
                        None,
                        "association_reflection",
                        format!(
                            "`association(:{}).{}` on {}: not ported for {why}",
                            name.as_str(),
                            method.as_str(),
                            owner.0.as_str()
                        ),
                    ));
                }
            }
            return;
        }
        // `record.<has_many>.reset.<m>`: Rails answers the reset proxy.
        if let Some(r) = recv {
            let ExprNode::Send { recv: Some(read), method: rm, args: rargs, .. } = &*r.node else { return };
            if rm.as_str() != "reset" || !rargs.is_empty() {
                return;
            }
            let ExprNode::Send { recv: owner_recv, method: assoc, args: aargs, block: None, .. } = &*read.node
            else {
                return;
            };
            if !aargs.is_empty() {
                return;
            }
            let implicit = owner_recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
            if implicit && enclosing.is_none() {
                return;
            }
            let owners = self.owners(owner_recv.as_ref(), enclosing);
            if !owners.is_empty()
                && owners.iter().all(|o| self.has_many.get(o).is_some_and(|names| names.contains(assoc)))
            {
                out.push(Diagnostic::unsupported(
                    e.span,
                    None,
                    "association_proxy_reset",
                    format!(
                        "`{}.reset.{}`: Rails answers the reset CollectionProxy, the emitted reset answers the association object",
                        assoc.as_str(),
                        method.as_str()
                    ),
                ));
            }
        }
    }
}

/// Why `call` on `assoc` is not ported, if it is not.
fn unported(assoc: &Association, call: &str) -> Option<&'static str> {
    match (assoc, call) {
        (Association::HasMany { through: Some(_), .. } | Association::HasAndBelongsToMany { .. }, "stale_target?") => {
            Some("a :through association")
        }
        (Association::BelongsTo { polymorphic: true, .. }, "stale_target?") => Some("a polymorphic belongs_to"),
        _ => None,
    }
}
