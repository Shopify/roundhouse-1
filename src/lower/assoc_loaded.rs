//! Flatten Rails' AssociationProxy spelling onto Roundhouse's flat
//! association surface.
//!
//! Roundhouse's has_many readers return a plain Array (cache hit) or run
//! a query (cache miss) — never Rails' AssociationProxy — so the Rails
//! spelling `message.boosts.loaded?` has nothing to dispatch on. The
//! synthesizer already exposes the flag as a flat predicate
//! (`message.boosts_loaded?`); rewrite the two-hop form onto that name
//! so analyze, emit, and the Spinel AOT all see an ordinary Bool method.
//!
//! Same for `association(:name).target` → `name`: Rails' reflection
//! API reaches the cached association object; Roundhouse's readers ARE
//! that object (or its Array), so the hop collapses onto the reader.
//! Campfire's FTS index update uses
//! `association(:rich_text_body).target&.saved_change_to_body?`.
//!
//! Same shape as `has_json`'s two-hop flatten (`account.settings.foo?` →
//! `account.settings_foo?`): the intermediate object Rails invents is
//! erased, and every target keeps a typed one-hop call.

use crate::det::{HashMap, HashSet};

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::dialect::Association;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

/// Per-model has_many names that have a synthesized `<name>_loaded?`
/// reader. Keyed by owner ClassId so `room.boosts.loaded?` is not
/// rewritten when only `Message` declares `has_many :boosts`.
pub(crate) fn has_many_by_model(app: &App) -> HashMap<ClassId, HashSet<Symbol>> {
    let mut out: HashMap<ClassId, HashSet<Symbol>> = HashMap::default();
    for model in &app.models {
        // Every model gets a key so `in_model` owner checks don't treat
        // a has_many-less model (Room) as a view/library class.
        let entry = out.entry(model.name.clone()).or_default();
        for (_, assoc) in model.spanned_associations() {
            // Only has_many synthesizes `<name>_loaded?` /
            // `<name>_target` (see `model_to_library::associations`).
            // has_one keeps a singular reader with no flat loaded flag.
            if let Association::HasMany { name, .. } = assoc {
                entry.insert(name.clone());
            }
        }
    }
    out
}

/// Per-model reader names `association(:x).target` may collapse onto
/// (`readers`), and which of them the model declares as associations
/// (`declared`; the rest are rich-text and plain-text readers).
pub(crate) struct AssociationReaders {
    pub(crate) readers: HashMap<ClassId, HashSet<Symbol>>,
    pub(crate) declared: HashMap<ClassId, HashSet<Symbol>>,
}

/// Per-model association / rich-text reader names `association(:x).target`
/// may collapse onto. Keyed by owner so `room.association(:boosts).target`
/// is not rewritten when only `Message` declares `:boosts`.
pub(crate) fn association_readers_by_model(app: &App) -> AssociationReaders {
    let mut out: HashMap<ClassId, HashSet<Symbol>> = HashMap::default();
    let mut declared: HashMap<ClassId, HashSet<Symbol>> = HashMap::default();
    for model in &app.models {
        let entry = out.entry(model.name.clone()).or_default();
        let declared = declared.entry(model.name.clone()).or_default();
        for (_, assoc) in model.spanned_associations() {
            entry.insert(assoc.name().clone());
            declared.insert(assoc.name().clone());
        }
        for (_, attr) in crate::lower::rich_text::rich_text_attrs(model) {
            entry.insert(Symbol::from(format!("rich_text_{}", attr.as_str())));
        }
        if crate::lower::plain_text_attr::record_table_present(&app.schema) {
            for (_, attr) in crate::lower::plain_text_attr::plain_text_attrs(model) {
                entry.insert(Symbol::from(format!("markdown_{}", attr.as_str())));
            }
        }
    }
    AssociationReaders { readers: out, declared }
}

/// Resolve which model owns the association hop.
///
/// Typed explicit receiver wins — including a union, but only when every
/// class alternative declares `name`. Untyped explicit receivers in a
/// **model** method are left alone (unrewritten `.loaded?` / `.target`
/// must stay visible). Views, tests, and other non-model owners still
/// unique-name: view assigns are untyped, and Campfire ERB is
/// `message.boosts.loaded?` (view bodies are library classes, so
/// `enclosing` is Some). Implicit self uses the enclosing model, or a
/// concern module's sole model includer.
fn resolve_owner_model(
    recv: Option<&Expr>,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    name: &Symbol,
) -> Option<ClassId> {
    if let Some(base) = recv {
        if let Some(id) = owner_from_typed_recv(base, by_model, name) {
            return Some(id);
        }
        // Unique-name for view/test/non-model owners only. Model methods
        // with an untyped or mixed-union receiver stay unre-written.
        // View locals are `Ty::Untyped`, not `ty: None`.
        let in_model = enclosing.is_some_and(|id| by_model.contains_key(id));
        if !in_model {
            return unique_model_for_name(by_model, name);
        }
        return None;
    }
    let enclosing = enclosing?;
    if by_model.contains_key(enclosing) {
        return Some(enclosing.clone());
    }
    let includer = sole_includer.get(enclosing)?;
    by_model.contains_key(includer).then(|| includer.clone())
}

/// ClassId of a typed explicit receiver. Mixed-owner unions rewrite only
/// when every class alternative declares `name`; a missing type or a
/// non-class type returns `None` (no unique-name fallback).
fn owner_from_typed_recv(
    expr: &Expr,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    name: &Symbol,
) -> Option<ClassId> {
    match expr.ty.as_ref()? {
        Ty::Class { id, .. } => Some(id.clone()),
        Ty::Union { variants } => {
            let ids: Vec<ClassId> = variants
                .iter()
                .filter_map(|v| match v {
                    Ty::Class { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                return None;
            }
            ids.iter()
                .all(|id| by_model.get(id).is_some_and(|names| names.contains(name)))
                .then(|| ids[0].clone())
        }
        _ => None,
    }
}

fn unique_model_for_name(
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    name: &Symbol,
) -> Option<ClassId> {
    let mut matches = by_model
        .iter()
        .filter(|(_, names)| names.contains(name))
        .map(|(id, _)| id);
    let first = matches.next().cloned();
    if matches.next().is_some() {
        None
    } else {
        first
    }
}

/// Rewrite every `recv.assoc.loaded?` whose `assoc` is a known has_many
/// **on the receiver's model** into `recv.assoc_loaded?`, and every
/// `association(:name).target` into a bare `name` reader when `name`
/// is a known association/rich-text reader **on the receiver's model**.
/// Implicit-self forms get a `self.` hop (same collapse `has_json` uses).
pub fn apply_assoc_loaded_lowering(app: &mut App) -> Vec<Diagnostic> {
    let by_model = has_many_by_model(app);
    let readers = association_readers_by_model(app);
    let sole_includer = app.sole_includer_of_modules();
    super::for_each_owned_hook_body(app, &mut |owner, e| {
        rewrite(e, owner, &sole_includer, &by_model, &readers);
    });
    for view in &mut app.views {
        rewrite(&mut view.body, None, &sole_includer, &by_model, &readers);
    }
    super::for_each_test_body(app, &mut |e| {
        rewrite(e, None, &sole_includer, &by_model, &readers);
    });
    Vec::new()
}

fn rewrite(
    expr: &mut Expr,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    readers: &AssociationReaders,
) {
    expr.node
        .for_each_child_mut(&mut |c| rewrite(c, enclosing, sole_includer, by_model, readers));
    rewrite_node(expr, enclosing, sole_includer, by_model, readers);
}

pub(crate) fn rewrite_node(
    expr: &mut Expr,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    readers: &AssociationReaders,
) {
    if rewrite_association_target(expr, enclosing, sole_includer, readers) {
        return;
    }
    let ExprNode::Send {
        recv: Some(inner),
        method,
        args,
        ..
    } = &*expr.node
    else {
        return;
    };
    let proxy_call = matches!(method.as_str(), "reload" | "reset");
    if (method.as_str() != "loaded?" && !proxy_call) || !args.is_empty() {
        return;
    }
    let ExprNode::Send {
        recv: owner,
        method: assoc,
        args: assoc_args,
        block: None,
        ..
    } = &*inner.node
    else {
        return;
    };
    if !assoc_args.is_empty() {
        return;
    }
    // Scope by the association receiver's model (`message` in
    // `message.boosts.loaded?`), not a global name set — a has_one or
    // plain method of the same name on another class must stay.
    let Some(owner_model) =
        resolve_owner_model(owner.as_ref(), enclosing, sole_includer, by_model, assoc)
    else {
        return;
    };
    let Some(names) = by_model.get(&owner_model) else {
        return;
    };
    if !names.contains(assoc) {
        return;
    }
    if proxy_call {
        // `record.comments.reload` / `.reset`: Rails' CollectionProxy
        // runs it on the association. Marked for the trees that carry
        // the association object (`IrHint::AssociationProxy`), with an
        // implicit-self owner spelled out so the call can pass it.
        if owner.is_none() {
            let mut s = Expr::new(inner.span, ExprNode::SelfRef);
            s.ty = Some(Ty::Class { id: owner_model, args: vec![] });
            let mut read = inner.clone();
            if let ExprNode::Send { recv, .. } = &mut *read.node {
                *recv = Some(s);
            }
            if let ExprNode::Send { recv: Some(r), .. } = &mut *expr.node {
                *r = read;
            }
        }
        expr.hint = Some(crate::expr::IrHint::AssociationProxy);
        return;
    }
    let flat = Symbol::from(format!("{}_loaded?", assoc.as_str()));
    // Explicit `message.boosts.loaded?` keeps `message` as receiver.
    // Implicit-self `boosts.loaded?` collapses to `self.boosts_loaded?`
    // — the same SelfRef hop `has_json` uses for `settings.foo?`.
    let new_recv = match owner {
        None => {
            let mut s = Expr::new(inner.span, ExprNode::SelfRef);
            s.ty = Some(Ty::Class {
                id: owner_model,
                args: vec![],
            });
            Some(s)
        }
        Some(base) => Some(base.clone()),
    };
    let mut rewritten = Expr::new(
        expr.span,
        ExprNode::Send {
            recv: new_recv,
            method: flat,
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    rewritten.ty = Some(Ty::Bool);
    *expr = rewritten;
}

/// `association(:rich_text_body).target` → `rich_text_body` (or
/// `self.rich_text_body` when the association call was implicit-self).
/// Scoped to the association call's receiver model so a name declared
/// only on an unrelated model does not rewrite onto this receiver.
fn rewrite_association_target(
    expr: &mut Expr,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    readers: &AssociationReaders,
) -> bool {
    let ExprNode::Send {
        recv: Some(inner),
        method,
        args,
        ..
    } = &*expr.node
    else {
        return false;
    };
    if method.as_str() != "target" || !args.is_empty() {
        return false;
    }
    let ExprNode::Send {
        recv: owner,
        method: assoc_method,
        args: assoc_args,
        ..
    } = &*inner.node
    else {
        return false;
    };
    if assoc_method.as_str() != "association" || assoc_args.len() != 1 {
        return false;
    }
    let Some(name) = sym_lit(&assoc_args[0]) else {
        return false;
    };
    let Some(owner_model) =
        resolve_owner_model(owner.as_ref(), enclosing, sole_includer, &readers.readers, &name)
    else {
        return false;
    };
    let Some(names) = readers.readers.get(&owner_model) else {
        return false;
    };
    let declared = readers.declared.get(&owner_model).is_some_and(|d| d.contains(&name));
    if !names.contains(&name) {
        return false;
    }
    let new_recv = match owner {
        None => {
            let mut s = Expr::new(inner.span, ExprNode::SelfRef);
            s.ty = Some(Ty::Class {
                id: owner_model,
                args: vec![],
            });
            Some(s)
        }
        Some(base) => Some(base.clone()),
    };
    // Preserve the analyzed type so a safe-nav chain on `.target`
    // (`….target&.saved_change_to_body?`) still has a typed receiver
    // for emit — `Expr::new` alone leaves `ty: None`.
    let prior_ty = expr.ty.clone();
    let mut rewritten = Expr::new(
        expr.span,
        ExprNode::Send {
            recv: new_recv,
            method: name,
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    rewritten.ty = prior_ty;
    // The CRuby and JRuby trees carry the association object, whose
    // `target` reads the loaded record(s) without loading
    // (runtime/spinel/active_record_associations.rb); they write a
    // declared association's read back (`emit::ruby::expr`). The other
    // trees keep the reader, which loads.
    if declared {
        rewritten.hint = Some(crate::expr::IrHint::AssociationTarget);
    }
    *expr = rewritten;
    true
}

/// Write each read `rewrite_association_target` hinted back as Rails
/// spelled it, `recv.association(:name).target`, for a tree that carries
/// the association object (the CRuby and JRuby trees; the caller gates).
/// Run ahead of the Ruby emit's own rewrites: those read a reader call
/// as an association read (a has_many reader continuing into `length`
/// seeds a Relation) and rebuild the node without its hint, so left to
/// the emitter the read would load instead of answering what is in
/// memory. The emitter writes back any hinted read this does not reach.
pub(crate) fn materialize_association_targets(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut |c| materialize_association_targets(c));
    if let Some(call) = association_proxy_call(expr) {
        let ty = expr.ty.clone();
        *expr = call;
        expr.ty = ty;
        return;
    }
    if expr.hint != Some(crate::expr::IrHint::AssociationTarget) {
        return;
    }
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return };
    if !args.is_empty() {
        return;
    }
    let span = expr.span;
    let name = Expr::new(span, ExprNode::Lit { value: Literal::Sym { value: method.clone() } });
    let association = Expr::new(
        span,
        ExprNode::Send {
            recv: recv.clone(),
            method: Symbol::from("association"),
            args: vec![name],
            block: None,
            parenthesized: true,
        },
    );
    let mut target = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(association),
            method: Symbol::from("target"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    target.ty = expr.ty.clone();
    *expr = target;
}

/// `owner.<has_many>.reload` / `.reset` marked `IrHint::AssociationProxy`
/// → `ActiveRecord::Associations::Association.proxy(owner, :<has_many>,
/// :reload)` (runtime/spinel/active_record_associations.rb), for a tree
/// that carries the association object (the caller gates).
pub(crate) fn association_proxy_call(expr: &Expr) -> Option<Expr> {
    if expr.hint != Some(crate::expr::IrHint::AssociationProxy) {
        return None;
    }
    let ExprNode::Send { recv: Some(inner), method: call, args, block: None, .. } = &*expr.node else {
        return None;
    };
    let ExprNode::Send { recv: Some(owner), method: name, args: read_args, block: None, .. } = &*inner.node
    else {
        return None;
    };
    if !args.is_empty() || !read_args.is_empty() {
        return None;
    }
    let span = expr.span;
    let sym = |s: &Symbol| Expr::new(span, ExprNode::Lit { value: Literal::Sym { value: s.clone() } });
    Some(Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(
                span,
                ExprNode::Const {
                    path: vec![
                        Symbol::from("ActiveRecord"),
                        Symbol::from("Associations"),
                        Symbol::from("Association"),
                    ],
                },
            )),
            method: Symbol::from("proxy"),
            args: vec![owner.clone(), sym(name), sym(call)],
            block: None,
            parenthesized: true,
        },
    ))
}

fn sym_lit(expr: &Expr) -> Option<Symbol> {
    match &*expr.node {
        ExprNode::Lit {
            value: Literal::Sym { value },
        } => Some(value.clone()),
        _ => None,
    }
}
