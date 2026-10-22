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

/// Per-model association names with a synthesized flat `<name>_loaded?`:
/// has_many, has_one, non-polymorphic belongs_to, and the
/// `rich_text_<attr>` has_one. `association(:name).loaded?` rewrites onto
/// it; any other name (a polymorphic belongs_to, habtm) stays put.
pub(crate) fn flat_loaded_by_model(app: &App) -> HashMap<ClassId, HashSet<Symbol>> {
    let mut out: HashMap<ClassId, HashSet<Symbol>> = HashMap::default();
    for model in &app.models {
        let entry = out.entry(model.name.clone()).or_default();
        for (_, assoc) in model.spanned_associations() {
            match assoc {
                Association::HasMany { name, .. }
                | Association::HasOne { name, .. }
                | Association::BelongsTo { name, polymorphic: false, .. } => {
                    entry.insert(name.clone());
                }
                _ => {}
            }
        }
        for (_, attr) in crate::lower::rich_text::rich_text_attrs(model) {
            entry.insert(Symbol::from(format!("rich_text_{}", attr.as_str())));
        }
        // `has_one_attached :<attr>` — its `<attr>_attachment_loaded?`.
        for (_, attr) in crate::lower::attached::attached_attrs(model) {
            entry.insert(crate::lower::attached::attachment_assoc_name(&attr));
        }
    }
    out
}

/// Concern modules whose includers are all models, each with the
/// has_many names every includer declares. Core's
/// `HasSyncedActiveRecordMetafields` declares `has_many :metafields` on
/// each includer from its `included` hook, and its own methods ask
/// `owner.metafields.loaded?` with `owner = self`: every includer
/// answers the flat `metafields_loaded?`, so the hop flattens in the
/// module body too, whichever includer runs it.
pub(crate) fn module_has_many(
    app: &App,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
) -> HashMap<ClassId, HashSet<Symbol>> {
    let mut out: HashMap<ClassId, HashSet<Symbol>> = HashMap::default();
    for (module, includers) in app.includers_of_modules() {
        let mut common: Option<HashSet<Symbol>> = None;
        for id in &includers {
            let Some(names) = by_model.get(id) else {
                common = None;
                break;
            };
            common = Some(match common {
                None => names.clone(),
                Some(c) => c.intersection(names).cloned().collect(),
            });
        }
        if let Some(names) = common.filter(|c| !c.is_empty()) {
            out.insert(module, names);
        }
    }
    out
}

/// A concern's private method named like an includer's synthesized
/// flat predicate (`metafields_loaded?(owner)` in
/// `HasSyncedActiveRecordMetafields`) is shadowed by it: the model's own
/// `def metafields_loaded?` wins over the included module's, and the
/// concern's calls fail with "given 1, expected 0". Rails has no such
/// method on the model (its predicate is `association(:x).loaded?`), so
/// the concern's method keeps its meaning under a name only it uses:
/// the def and its implicit-self calls in the module and its model
/// includers move to `<module>__<name>`. Private, so no explicit
/// receiver elsewhere can name it. The flat predicate keeps its one
/// name for every other pass.
pub(crate) fn rename_shadowed_module_predicates(app: &mut App) {
    let flat_loaded = flat_loaded_by_model(app);
    let includers = app.includers_of_modules();
    let mut renames: Vec<(ClassId, Vec<ClassId>, Symbol, Symbol)> = Vec::new();
    for lc in app.library_classes.iter().filter(|lc| lc.is_module) {
        let Some(users) = includers.get(&lc.name) else { continue };
        for m in &lc.methods {
            if m.receiver != crate::dialect::MethodReceiver::Instance
                || m.visibility != crate::dialect::MethodVisibility::Private
                || m.name_span.is_synthetic()
            {
                continue;
            }
            let Some(assoc) = m.name.as_str().strip_suffix("_loaded?") else { continue };
            let assoc = Symbol::from(assoc);
            if !users
                .iter()
                .any(|id| flat_loaded.get(id).is_some_and(|names| names.contains(&assoc)))
            {
                continue;
            }
            // An includer's own def overrides the concern's in Rails too.
            if app.models.iter().filter(|model| users.contains(&model.name)).any(|model| {
                model.body.iter().any(|item| {
                    matches!(item, crate::dialect::ModelBodyItem::Method { method, .. }
                        if method.name == m.name
                            && method.receiver == crate::dialect::MethodReceiver::Instance)
                })
            }) {
                continue;
            }
            let prefix = crate::naming::snake_case(&lc.name.0.as_str().replace("::", ""));
            let renamed = Symbol::from(format!("{prefix}__{}", m.name.as_str()));
            renames.push((lc.name.clone(), users.iter().cloned().collect(), m.name.clone(), renamed));
        }
    }
    for (module, users, from, to) in renames {
        fn rename_calls(e: &mut Expr, from: &Symbol, to: &Symbol) {
            e.node.for_each_child_mut(&mut |c| rename_calls(c, from, to));
            if let ExprNode::Send { recv, method, .. } = &mut *e.node {
                let implicit = match recv {
                    None => true,
                    Some(r) => matches!(&*r.node, ExprNode::SelfRef),
                };
                if implicit && method == from {
                    *method = to.clone();
                }
            }
        }
        if let Some(lc) = app.library_classes.iter_mut().find(|lc| lc.name == module) {
            for m in &mut lc.methods {
                if m.name == from && m.receiver == crate::dialect::MethodReceiver::Instance {
                    m.name = to.clone();
                }
                rename_calls(&mut m.body, &from, &to);
            }
        }
        for model in app.models.iter_mut().filter(|m| users.contains(&m.name)) {
            for item in &mut model.body {
                if let crate::dialect::ModelBodyItem::Method { method, .. } = item {
                    if method.receiver == crate::dialect::MethodReceiver::Instance {
                        rename_calls(&mut method.body, &from, &to);
                    }
                }
            }
        }
    }
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
    let flat_loaded = flat_loaded_by_model(app);
    let sole_includer = app.sole_includer_of_modules();
    let modules = module_has_many(app, &by_model);
    rename_shadowed_module_predicates(app);
    super::for_each_owned_hook_body(app, &mut |owner, e| {
        rewrite(e, owner, &sole_includer, &by_model, &modules, &readers, &flat_loaded);
    });
    for view in &mut app.views {
        rewrite(&mut view.body, None, &sole_includer, &by_model, &modules, &readers, &flat_loaded);
    }
    super::for_each_test_body(app, &mut |e| {
        rewrite(e, None, &sole_includer, &by_model, &modules, &readers, &flat_loaded);
    });
    Vec::new()
}

fn rewrite(
    expr: &mut Expr,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    modules: &HashMap<ClassId, HashSet<Symbol>>,
    readers: &AssociationReaders,
    flat_loaded: &HashMap<ClassId, HashSet<Symbol>>,
) {
    expr.node.for_each_child_mut(&mut |c| {
        rewrite(c, enclosing, sole_includer, by_model, modules, readers, flat_loaded)
    });
    rewrite_node(expr, enclosing, sole_includer, by_model, modules, readers, flat_loaded);
}

pub(crate) fn rewrite_node(
    expr: &mut Expr,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    modules: &HashMap<ClassId, HashSet<Symbol>>,
    readers: &AssociationReaders,
    flat_loaded: &HashMap<ClassId, HashSet<Symbol>>,
) {
    if let Some(mut unrolled) = unroll_all_loaded(expr) {
        rewrite(&mut unrolled, enclosing, sole_includer, by_model, modules, readers, flat_loaded);
        *expr = unrolled;
        return;
    }
    if rewrite_association_target(expr, enclosing, sole_includer, readers) {
        return;
    }
    if rewrite_association_loaded(expr, enclosing, sole_includer, flat_loaded, &readers.declared) {
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
    // `record.comments.target`: CollectionProxy#target, the loaded (or
    // built) records without a load — the synthesized `comments_target`.
    let target_read = method.as_str() == "target";
    if (method.as_str() != "loaded?" && !proxy_call && !target_read) || !args.is_empty() {
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
    let declares = |id: &ClassId| by_model.get(id).is_some_and(|names| names.contains(assoc));
    let resolved = resolve_owner_model(owner.as_ref(), enclosing, sole_includer, by_model, assoc)
        .filter(|id| declares(id))
        .or_else(|| module_owner(owner.as_ref(), enclosing, modules, assoc));
    // A view/test receiver whose model the name does not single out
    // (`widget.parts.loaded?` in a partial, when Widget and Gadget both
    // `has_many :parts`): the same widening `association(:name).loaded?`
    // takes below. Every declaring model synthesizes the same flat
    // predicate, so the spelling is right whichever one the receiver is.
    let stamp = match (&resolved, owner) {
        (None, Some(base)) => match widened_owner(base, enclosing, by_model, assoc) {
            Some(id) => Some(id),
            None => return,
        },
        (None, None) => return,
        (Some(_), _) => None,
    };
    let Some(owner_model) = resolved.or_else(|| stamp.clone()) else {
        return;
    };
    if proxy_call {
        // `record.comments.reload` / `.reset`: Rails' CollectionProxy
        // runs it on the association. Marked for the trees that carry
        // the association object (`IrHint::AssociationProxy`), with an
        // implicit-self owner spelled out so the call can pass it.
        if owner.is_none() {
            let mut s = Expr::new(inner.span, ExprNode::SelfRef);
            s.ty = Some(Ty::Class { id: owner_model, args: vec![].into() });
            let mut read = inner.clone();
            if let ExprNode::Send { recv, .. } = &mut *read.node {
                *recv = Some(s);
            }
            if let ExprNode::Send { recv: Some(r), .. } = &mut *expr.node {
                *r = read;
            }
        }
        expr.set_hint(Some(crate::expr::IrHint::AssociationProxy));
        return;
    }
    let flat = if target_read {
        Symbol::from(format!("{}_target", assoc.as_str()))
    } else {
        Symbol::from(format!("{}_loaded?", assoc.as_str()))
    };
    // Explicit `message.boosts.loaded?` keeps `message` as receiver.
    // Implicit-self `boosts.loaded?` collapses to `self.boosts_loaded?`
    // — the same SelfRef hop `has_json` uses for `settings.foo?`.
    let new_recv = match owner {
        None => {
            let mut s = Expr::new(inner.span, ExprNode::SelfRef);
            s.ty = Some(Ty::Class {
                id: owner_model,
                args: vec![].into(),
            });
            Some(s)
        }
        Some(base) => {
            let mut recv = base.clone();
            if let Some(id) = stamp {
                recv.ty = Some(Ty::Class { id, args: vec![].into() });
            }
            Some(recv)
        }
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
    rewritten.ty = if target_read { expr.ty.clone() } else { Some(Ty::Bool) };
    *expr = rewritten;
}

/// In a module `module_has_many` admits, the module owns `assoc` when
/// the hop's receiver is its self: implicit, `self`, or a local the
/// analyzer typed as the module or left untyped (`owner = self` under
/// `#: self as untyped`).
fn module_owner(
    recv: Option<&Expr>,
    enclosing: Option<&ClassId>,
    modules: &HashMap<ClassId, HashSet<Symbol>>,
    assoc: &Symbol,
) -> Option<ClassId> {
    let module = enclosing?;
    if !modules.get(module).is_some_and(|names| names.contains(assoc)) {
        return None;
    }
    let admits = match recv {
        None => true,
        Some(r) if matches!(&*r.node, ExprNode::SelfRef) => true,
        Some(r) => match r.ty.as_ref() {
            None | Some(Ty::Untyped) => true,
            Some(Ty::Class { id, .. }) => id == module,
            _ => false,
        },
    };
    admits.then(|| module.clone())
}

/// The model to stamp on a view/test receiver of `<recv>.<name>.loaded?`
/// (or `.reload`) that `resolve_owner_model` could not single out: some
/// model declares `has_many :<name>`, and the receiver is not typed as
/// (or as a union with) a model that lacks it. `None` in a model method,
/// where an unresolved receiver stays as written. The first declaring
/// model by name, so the stamp does not depend on map order.
fn widened_owner(
    base: &Expr,
    enclosing: Option<&ClassId>,
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    name: &Symbol,
) -> Option<ClassId> {
    if enclosing.is_some_and(|id| by_model.contains_key(id)) {
        return None;
    }
    let typed_as_model = |ty: &Ty| matches!(ty, Ty::Class { id, .. } if by_model.contains_key(id));
    let refused = match base.ty.as_ref() {
        Some(ty @ Ty::Class { .. }) => typed_as_model(ty),
        // A union with a model alternative that lacks the name (the
        // others' would have resolved it already).
        Some(Ty::Union { variants }) => variants.iter().any(typed_as_model),
        _ => false,
    };
    if refused {
        return None;
    }
    by_model
        .iter()
        .filter(|(_, names)| names.contains(name))
        .map(|(id, _)| id)
        .min_by(|a, b| a.0.as_str().cmp(b.0.as_str()))
        .cloned()
}

/// True when at least one model synthesizes `<name>_loaded?`.
fn any_model_declares_name(
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    name: &Symbol,
) -> bool {
    by_model.values().any(|names| names.contains(name))
}

/// A ClassId that synthesizes `<name>_loaded?`, for stamping the
/// rewritten send's receiver type on strict targets. When several
/// models share the name (Message and Membership both `:room`), any
/// declaring class is enough — the spelling is identical.
fn a_model_declaring(
    by_model: &HashMap<ClassId, HashSet<Symbol>>,
    name: &Symbol,
) -> Option<ClassId> {
    by_model
        .iter()
        .find(|(_, names)| names.contains(name))
        .map(|(id, _)| id.clone())
}

/// `association(:creator).loaded?` → `creator_loaded?` (or
/// `self.creator_loaded?` when implicit-self), for every association
/// with a synthesized flat predicate on the receiver's model.
fn rewrite_association_loaded(
    expr: &mut Expr,
    enclosing: Option<&ClassId>,
    sole_includer: &HashMap<ClassId, ClassId>,
    flat_loaded: &HashMap<ClassId, HashSet<Symbol>>,
    declared: &HashMap<ClassId, HashSet<Symbol>>,
) -> bool {
    let ExprNode::Send { recv: Some(inner), method, args, .. } = &*expr.node else {
        return false;
    };
    if method.as_str() != "loaded?" || !args.is_empty() {
        return false;
    }
    let ExprNode::Send { recv: owner, method: assoc_method, args: assoc_args, .. } = &*inner.node
    else {
        return false;
    };
    if assoc_method.as_str() != "association" || assoc_args.len() != 1 {
        return false;
    }
    let Some(name) = sym_lit(&assoc_args[0]) else {
        return false;
    };
    let flat = Symbol::from(format!("{}_loaded?", name.as_str()));
    let in_model = enclosing.is_some_and(|id| flat_loaded.contains_key(id));
    // Test/view explicit receivers: unique-name fails when Message and
    // Membership both `belongs_to :room`, and `message.reload` often
    // types as `ActiveRecord::Base` so typed resolve has no flat
    // `room_loaded?`. Name existence on *some* model gates the widen,
    // but a known receiver model that itself lacks the flat predicate
    // (polymorphic `belongs_to`) keeps the dynamic path. Stamp a
    // declaring ClassId on the kept receiver for strict targets.
    // Model methods and implicit-self keep the stricter resolve below.
    if let Some(base) = owner {
        if !in_model {
            let typed = owner_from_typed_recv(base, flat_loaded, &name);
            let allow = match &typed {
                // `owner_from_typed_recv` only returns Some when every
                // class alt declares `name` — or a single Class id. A
                // concrete model key that omits the name (poly assoc)
                // must not rewrite.
                Some(id) if flat_loaded.contains_key(id) => {
                    flat_loaded.get(id).is_some_and(|n| n.contains(&name))
                }
                // Base / untyped / unknown: any declaring model is enough.
                // An untyped receiver may be the model whose `name` is a
                // polymorphic `belongs_to` (Sale's `source`, beside another
                // model's plain one): it has no flat predicate, so every
                // model that declares `name` must have one.
                _ => {
                    any_model_declares_name(flat_loaded, &name)
                        && declared
                            .iter()
                            .filter(|(_, names)| names.contains(&name))
                            .all(|(id, _)| flat_loaded.get(id).is_some_and(|n| n.contains(&name)))
                }
            };
            if allow {
                let mut recv = base.clone();
                let stamp = typed
                    .filter(|id| {
                        flat_loaded.get(id).is_some_and(|n| n.contains(&name))
                    })
                    .or_else(|| a_model_declaring(flat_loaded, &name));
                if let Some(id) = stamp {
                    recv.ty = Some(Ty::Class { id, args: vec![].into() });
                }
                let mut rewritten = Expr::new(
                    expr.span,
                    ExprNode::Send {
                        recv: Some(recv),
                        method: flat,
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                );
                rewritten.ty = Some(Ty::Bool);
                *expr = rewritten;
                return true;
            }
        }
    }
    // The unique-name fallback of an untyped receiver is no guess when a
    // sibling model declares `name` without a flat predicate.
    if owner.as_ref().is_some_and(|base| owner_from_typed_recv(base, flat_loaded, &name).is_none())
        && declared
            .iter()
            .any(|(id, names)| names.contains(&name) && !flat_loaded.get(id).is_some_and(|n| n.contains(&name)))
    {
        return false;
    }
    let Some(owner_model) = resolve_owner_model(
        owner.as_ref(),
        enclosing,
        sole_includer,
        flat_loaded,
        &name,
    ) else {
        return false;
    };
    if !flat_loaded
        .get(&owner_model)
        .is_some_and(|names| names.contains(&name))
    {
        return false;
    }
    let new_recv = match owner {
        None => {
            let mut s = Expr::new(inner.span, ExprNode::SelfRef);
            s.ty = Some(Ty::Class { id: owner_model, args: vec![].into() });
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
    true
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
                args: vec![].into(),
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
        rewritten.set_hint(Some(crate::expr::IrHint::AssociationTarget));
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
    if *expr.hint() != Some(crate::expr::IrHint::AssociationTarget) {
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
    if *expr.hint() != Some(crate::expr::IrHint::AssociationProxy) {
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

/// `%i[a b].all? { |name| recv.association(name).loaded? }` →
/// `recv.association(:a).loaded? && recv.association(:b).loaded?`, which
/// the literal-name rewrite then flattens onto `recv.a_loaded? &&
/// recv.b_loaded?`. campfire's `message_fragment_cache_key` asks this
/// way; with the name a block local, nothing could resolve it, and the
/// emitted `association(name)` had no method behind it.
fn unroll_all_loaded(expr: &Expr) -> Option<Expr> {
    let ExprNode::Send { recv: Some(list), method, args, block: Some(block), .. } = &*expr.node else {
        return None;
    };
    if method.as_str() != "all?" || !args.is_empty() {
        return None;
    }
    let ExprNode::Array { elements, .. } = &*list.node else {
        return None;
    };
    let names: Vec<Symbol> = elements.iter().map(sym_lit).collect::<Option<_>>()?;
    if names.is_empty() {
        return None;
    }
    let ExprNode::Lambda { params, body, .. } = &*block.node else {
        return None;
    };
    let [param] = params.as_slice() else {
        return None;
    };
    let ExprNode::Send { recv: Some(assoc_call), method: loaded, args: loaded_args, .. } = &*body.node else {
        return None;
    };
    if loaded.as_str() != "loaded?" || !loaded_args.is_empty() {
        return None;
    }
    let ExprNode::Send { recv: owner, method: assoc, args: assoc_args, block: None, .. } = &*assoc_call.node else {
        return None;
    };
    if assoc.as_str() != "association" {
        return None;
    }
    match assoc_args.as_slice() {
        [arg] if matches!(&*arg.node, ExprNode::Var { name, .. } if name == param) => {}
        _ => return None,
    }
    // The receiver must not read the block local itself.
    if owner.as_ref().is_some_and(|o| mentions_var(o, param)) {
        return None;
    }
    let term = |name: &Symbol| {
        let mut call = assoc_call.clone();
        if let ExprNode::Send { args, .. } = &mut *call.node {
            args[0] = Expr::new(args[0].span, ExprNode::Lit { value: Literal::Sym { value: name.clone() } });
        }
        let mut e = Expr::new(
            body.span,
            ExprNode::Send { recv: Some(call), method: loaded.clone(), args: vec![], block: None, parenthesized: false },
        );
        e.ty = Some(Ty::Bool);
        e
    };
    let mut out = term(&names[0]);
    for name in &names[1..] {
        let mut e = Expr::new(
            expr.span,
            ExprNode::BoolOp {
                op: crate::expr::BoolOpKind::And,
                surface: crate::expr::BoolOpSurface::Symbol,
                left: out,
                right: term(name),
            },
        );
        e.ty = Some(Ty::Bool);
        out = e;
    }
    Some(out)
}

fn mentions_var(e: &Expr, name: &Symbol) -> bool {
    if matches!(&*e.node, ExprNode::Var { name: n, .. } if n == name) {
        return true;
    }
    let mut found = false;
    e.node.for_each_child(&mut |c| found = found || mentions_var(c, name));
    found
}

fn sym_lit(expr: &Expr) -> Option<Symbol> {
    match &*expr.node {
        ExprNode::Lit {
            value: Literal::Sym { value },
        } => Some(value.clone()),
        _ => None,
    }
}
