//! `Rooms::Open.count` — an STI SUBCLASS standing at a query root.
//!
//! Rails' single-table inheritance gives a subclass its own default
//! scope: `Rooms::Open.count` counts the rooms whose `type` column says
//! `"Rooms::Open"`, not every row in `rooms`.
//!
//! An STI subclass is not ingested as a Model (it declares no table of
//! its own), so every one of those call sites resolved by plain Ruby
//! inheritance to the BASE class's method and answered the whole table.
//! campfire has seven rooms across three subclasses, so
//! `Rooms::Open.count` answered 7 where Rails answers 2 — a silently
//! wrong number, and the reason `user_test`'s membership-grant assertion
//! could pass while nothing was scoped: it compares that expression
//! against itself.
//!
//! The rewrite is a call-site one, into vocabulary every target already
//! speaks:
//!
//! ```text
//! Rooms::Open.pluck(:id)  ->  Room.where(type: "Rooms::Open").pluck(:id)
//! Rooms::Open.all         ->  Room.where(type: "Rooms::Open")
//! ```
//!
//! Synthesizing per-subclass class methods was the alternative and is
//! worse twice over: the surface is the whole Relation API, and each
//! method would have to land on every target.
//!
//! **`becomes!` is the one place a synthesized class method IS right.**
//! `room.becomes!(Rooms::Open)` recasts a loaded row to an STI sibling,
//! so unlike the relation surface it cannot be expressed by re-rooting
//! the call — the RESULT has to be an instance of the named subclass
//! (campfire's `Rooms::Open#grant_access_to_all_users` is a callback
//! that only exists on the sibling, and only fires if the object is
//! one). The target is always a literal constant at the call site, so
//! no runtime dispatch is needed: the call becomes
//! `Rooms::Open.becomes_from(room)`, and this pass pushes that class
//! method onto the subclass. Its return type is the subclass, which is
//! what keeps `@room = @room.becomes!(Rooms::Closed)` assignable on the
//! strict targets — a shared-runtime instance method could only have
//! returned `ActiveRecord::Base`.
//!
//! The COLUMN COPY is written out here too, one named column at a
//! time, and that placement was paid for: a shared-runtime
//! `Base#becomes_state_from(source)` looping over `source.attributes`
//! compiles on the ruby target and does not exist on Rust, where
//! `Base` is a plain struct and `attributes` lives on each model. The
//! generic loop needs the polymorphic dispatch back to the subclass
//! that the strict targets are built to avoid. Here the schema is
//! known at lowering time, so the loop unrolls into concrete `[]`/`[]=`
//! reads and writes and nothing needs dispatching.
//!
//! **Two halves.** The RELATION surface filters; the CONSTRUCTORS
//! (`new`/`create!`/…) STAMP, folding `type: "Rooms::Open"` into the
//! attribute hash — without it `Rooms::Open.create!` wrote a row
//! belonging to no subclass, which the read half above then correctly
//! could not see. A hand-written class method on the subclass
//! (`Rooms::Direct.find_or_create_for`) is the subclass's own; and
//! `find` is left alone because the arel pass inlines `<Model>.find` to
//! a primary-key SELECT — re-rooting it would move that work onto the
//! Relation path for no scoping gain a primary key can use.
//!
//! **A subclass with declarations of its own keeps its root.** A
//! subclass body call its model claims (`Model::claimed_body_calls`:
//! Core's `model_uuid_attributes` in a subclass) runs on the subclass
//! and declares attribute types there alone, and a relation binds its
//! values through its own model's types. Re-rooted on the base,
//! `SpecialItem.where(ref: 66)` bound 66 as the base's uuid column. Such
//! a subclass scopes on itself: `SpecialItem.where(type: "SpecialItem")`,
//! which the runtime Relation runs with the subclass as its model.

use crate::det::{HashMap, HashSet};

use crate::app::App;
use crate::dialect::{AccessorKind, MethodDef, MethodReceiver, Param};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::span::Span;
use crate::ty::Ty;

/// The Relation entry points a subclass inherits and must scope. `all`
/// is handled separately (it IS the scope, so the call disappears).
const STI_RELATION_METHODS: &[&str] = &[
    "all", "count", "pluck", "ids", "where", "find_by", "find_by!", "first", "last", "exists?",
    "any?", "none?", "empty?", "order", "limit", "offset", "joins", "includes", "distinct",
    "group", "maximum", "minimum", "destroy_all", "delete_all", "update_all",
];

/// Construction on a subclass, which has to STAMP the type column
/// rather than filter on it. `Rooms::Open.create!(name: …)` wrote a row
/// with an empty `type`, so it belonged to no subclass at all — and the
/// read half above made that visible, because a correctly-scoped
/// `Rooms::Open.count` then cannot see the row it just created.
const STI_CONSTRUCTORS: &[&str] = &["new", "create", "create!", "build"];

/// Rails' inheritance column. Not configurable here — `inheritance_column`
/// is a per-model override no corpus app writes, and guessing at one
/// would filter on a column that isn't there.
const INHERITANCE_COLUMN: &str = "type";

/// `self.inheritance_column = "kind"` / `:kind` / `nil` in a model body:
/// the literal, empty for `nil` (Rails' writer stores `value.to_s`).
pub(crate) fn inheritance_column_write(expr: &Expr) -> Option<String> {
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*expr.node else { return None };
    if !matches!(&*recv.node, ExprNode::SelfRef) || method.as_str() != "inheritance_column=" {
        return None;
    }
    match args.as_slice() {
        [arg] => match &*arg.node {
            ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
            ExprNode::Lit { value: Literal::Nil } => Some(String::new()),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a model-body statement is a literal `inheritance_column`
/// write (`model_to_library::MODEL_STATEMENT_CLAIMS`): read here for the
/// STI column, and answered at run time by the class method
/// [`push_inheritance_column_method`] writes.
pub(crate) fn claims_inheritance_column(expr: &Expr) -> bool {
    inheritance_column_write(expr).is_some()
}

/// The literal `inheritance_column` write in `model`'s own body, the
/// last one winning.
fn own_inheritance_column(model: &crate::dialect::Model) -> Option<String> {
    model
        .body
        .iter()
        .filter_map(|item| match item {
            crate::dialect::ModelBodyItem::Unknown { expr, .. }
            | crate::dialect::ModelBodyItem::ClassBody { expr, .. } => inheritance_column_write(expr),
            _ => None,
        })
        .last()
}

/// `model`'s inheritance column (a class attribute): its own literal
/// setting or the nearest model ancestor's, else `type`. Empty when set
/// to `nil`; a name the table lacks (`:_type_disabled`) disables STI
/// just as well, since [`sti_bases`] asks the table for it.
pub(crate) fn inheritance_column(model: &crate::dialect::Model, models: &[crate::dialect::Model]) -> String {
    let mut cursor = Some(model);
    for _ in 0..16 {
        let Some(m) = cursor else { break };
        if let Some(col) = own_inheritance_column(m) {
            return col;
        }
        cursor = m.parent.as_ref().and_then(|p| models.iter().find(|o| &o.name == p));
    }
    INHERITANCE_COLUMN.to_string()
}

/// `def self.inheritance_column = "kind"` on a model that sets it (Ruby
/// family, beside the default-scope class methods): what the runtime's
/// `inheritance_column` reader (`Serialization`'s except) answers, the
/// subclasses inheriting it.
pub(crate) fn push_inheritance_column_method(methods: &mut Vec<MethodDef>, model: &crate::dialect::Model) {
    let Some(col) = own_inheritance_column(model) else { return };
    if methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == "inheritance_column") {
        return;
    }
    let mut body = Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Str { value: col } });
    body.ty = Some(Ty::Str);
    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from("inheritance_column"),
        receiver: MethodReceiver::Class,
        params: Vec::new(),
        body,
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    });
}

/// A model's subclass on a table with no inheritance column (no STI)
/// loads as the relation's class, as Rails' `instantiate` does on its
/// receiver: `MarkdownRule.first` is a `MarkdownRule`, with its methods.
/// The model's synthesized loaders name the model (`Rule.from_stmt`,
/// `Rule.new`); for a model with such subclasses they call through the
/// receiver class instead, which a subclass's inherited loader is.
/// Ruby family (`emit::ruby`, beside `apply_sti_hydration`): a strict
/// target's subclass is no subtype to return. Models without plain
/// subclasses are left byte-identical.
pub(crate) fn apply_plain_subclass_hydration(lcs: &mut [crate::dialect::LibraryClass], app: &App) {
    let sti = sti_bases(app);
    let models: HashSet<&ClassId> = app.models.iter().map(|m| &m.name).collect();
    let mut plain: HashSet<ClassId> = HashSet::default();
    // The subclasses are the app's library classes (as `sti_bases`
    // reads them); `lcs` are the lowered models.
    for lc in &app.library_classes {
        if sti.contains_key(&lc.name) {
            continue;
        }
        if let Some(parent) = lc.parent.as_ref().filter(|p| models.contains(p)) {
            plain.insert(parent.clone());
        }
    }
    if plain.is_empty() {
        return;
    }
    for lc in lcs.iter_mut() {
        if !plain.contains(&lc.name) {
            continue;
        }
        let owner = lc.name.clone();
        for m in &mut lc.methods {
            if m.receiver != MethodReceiver::Class || !m.name_span.is_synthetic() {
                continue;
            }
            let factory = matches!(m.name.as_str(), "from_stmt" | "from_row");
            through_receiver(&mut m.body, &owner, factory);
        }
    }
}

/// Plain (non-STI) model subclass -> the model its `parent` chain
/// reaches: `class BigAsset < Asset` on a table with no inheritance
/// column. Rails answers `BigAsset.select(:id)` / `.order(:id)` from a
/// relation on the subclass (`Querying` delegates them to `all`), so
/// `scope_chain` seeds `ActiveRecord::Relation.new(BigAsset)` for these
/// as it does `Relation.new(Asset)` for the model, and the rows load as
/// the subclass (`apply_plain_subclass_hydration`).
pub(crate) fn plain_model_subclasses(app: &App) -> HashMap<ClassId, ClassId> {
    let sti = sti_bases(app);
    let models: HashSet<&ClassId> = app.models.iter().map(|m| &m.name).collect();
    let mut lc_parent: HashMap<&ClassId, &Option<ClassId>> = HashMap::default();
    for other in &app.library_classes {
        lc_parent.entry(&other.name).or_insert(&other.parent);
    }
    let mut out = HashMap::default();
    for lc in &app.library_classes {
        if sti.contains_key(&lc.name) || models.contains(&lc.name) {
            continue;
        }
        let mut cursor = lc.parent.clone();
        for _ in 0..8 {
            let Some(parent) = cursor else { break };
            if models.contains(&parent) {
                out.insert(lc.name.clone(), parent);
                break;
            }
            cursor = lc_parent.get(&parent).and_then(|p| (*p).clone());
        }
    }
    out
}

/// `Owner.from_stmt(..)` / `Owner.from_row(..)` / `Owner.instantiate(..)`
/// (and, inside a factory, `Owner.new(..)`) -> the implicit-self call.
fn through_receiver(expr: &mut Expr, owner: &ClassId, factory: bool) {
    expr.node.for_each_child_mut(&mut |c| through_receiver(c, owner, factory));
    let ExprNode::Send { recv, method, .. } = &mut *expr.node else { return };
    let loader = matches!(method.as_str(), "from_stmt" | "from_row" | "instantiate") || (factory && method.as_str() == "new");
    if !loader {
        return;
    }
    let names_owner = recv.as_ref().is_some_and(|r| match &*r.node {
        ExprNode::Const { path } => {
            path.iter().map(|s| s.as_str().trim_start_matches("::")).collect::<Vec<_>>().join("::") == owner.0.as_str()
        }
        _ => false,
    });
    if names_owner {
        *recv = None;
    }
}

/// Each STI class's inheritance column, keyed by the subclasses
/// ([`sti_bases`]) and by their base models.
pub(crate) fn sti_columns(app: &App) -> HashMap<ClassId, Symbol> {
    let mut out: HashMap<ClassId, Symbol> = HashMap::default();
    for (sub, base) in sti_bases(app) {
        let Some(model) = app.models.iter().find(|m| m.name == base) else { continue };
        let col = Symbol::from(inheritance_column(model, &app.models));
        out.insert(sub, col.clone());
        out.insert(base, col);
    }
    out
}

/// The inheritance column of `class` (an STI subclass or base), `type`
/// when the map has none.
fn column_of<'a>(cols: &'a HashMap<ClassId, Symbol>, class: &ClassId) -> &'a str {
    cols.get(class).map(|c| c.as_str()).unwrap_or(INHERITANCE_COLUMN)
}

/// The synthesized recast entry point `becomes!` lowers to.
const BECOMES_FROM: &str = "becomes_from";

pub fn apply_sti_scope_lowering(app: &mut App) {
    let bases = sti_bases(app);
    // A model's subclass on a table with no `type` column is no STI
    // subclass: its relations carry no type condition, yet Rails applies
    // the subclass's own default scope to them, after the model's. Its
    // inherited relation entry points run on the subclass already.
    let models: HashSet<ClassId> = app.models.iter().map(|m| m.name.clone()).collect();
    for lc in &mut app.library_classes {
        if !bases.contains_key(&lc.name) && lc.parent.as_ref().is_some_and(|p| models.contains(p)) {
            super::default_scope::push_subclass_methods(lc);
        }
    }
    if bases.is_empty() {
        return;
    }
    // Subclasses a `becomes!` call site actually names. Only these grow
    // the synthesized `becomes_from`: a class method nobody calls is
    // dead weight on every target, and on the strict ones it is dead
    // weight that still has to type-check.
    let mut recast: HashSet<ClassId> = HashSet::default();
    let types = type_names(app, &bases);
    let named = runtime_sti_named(app);
    // Inside a subclass's OWN class method the receiver is implicit:
    // campfire's `Rooms::Direct.find_for` writes `all.joins(:users)
    // .detect { … }`, and Rails scopes that `all` to the subclass. Left
    // bare, it resolved to the base's and searched EVERY room, so the
    // open room both users belong to matched "the direct room with
    // exactly these members" — starting a DM redirected into it and no
    // direct room was ever created (found on the Fly deploy). Naming the
    // subclass here hands the call to the rewrite below.
    for lc in &mut app.library_classes {
        if !bases.contains_key(&lc.name) {
            continue;
        }
        let sub = lc.name.clone();
        for m in &mut lc.methods {
            if matches!(m.receiver, MethodReceiver::Class) {
                name_implicit_receiver(&mut m.body, &sub);
            }
        }
    }
    // Subclasses with declarations of their own root on themselves, as
    // does one with a default scope of its own: its relation runs the
    // subclass's `_apply_default_scope` (`lower::default_scope`).
    let roots: HashMap<ClassId, ClassId> = bases
        .iter()
        .map(|(sub, base)| {
            let own_scope = app
                .library_classes
                .iter()
                .find(|lc| &lc.name == sub)
                .is_some_and(super::default_scope::subclass_declares)
                || super::subclass_macros::declares_scopes(app, sub);
            let root = if own_scope || declares_own_types(app, sub, base) { sub } else { base };
            (sub.clone(), root.clone())
        })
        .collect();
    for lc in &mut app.library_classes {
        if bases.contains_key(&lc.name) {
            super::default_scope::push_subclass_methods(lc);
        }
    }
    let cols = sti_columns(app);
    super::for_each_hook_body(app, &mut |e| rewrite(e, &roots, &types, &named, &cols, &mut recast));
    for view in &mut app.views {
        rewrite(&mut view.body, &roots, &types, &named, &cols, &mut recast);
    }
    for tm in &mut app.test_modules {
        if let Some(setup) = &mut tm.setup {
            rewrite(setup, &roots, &types, &named, &cols, &mut recast);
        }
        for t in &mut tm.tests {
            rewrite(&mut t.body, &roots, &types, &named, &cols, &mut recast);
        }
        for m in &mut tm.helpers {
            rewrite(&mut m.body, &roots, &types, &named, &cols, &mut recast);
        }
    }
    push_becomes_from(app, &bases, &recast);
    stamp_sti_subclasses(app, &bases);
}

/// Whether the STI subclass `sub` has a body call its base model claims
/// (`Model::claimed_body_calls`), which runs on the subclass as written.
fn declares_own_types(app: &App, sub: &ClassId, base: &ClassId) -> bool {
    let Some(model) = app.models.iter().find(|m| &m.name == base) else { return false };
    app.library_classes
        .iter()
        .find(|lc| &lc.name == sub)
        .is_some_and(|lc| lc.unknown_calls.iter().any(|c| model.claimed_body_calls.contains(&c.span)))
}

/// Record each base model's STI subclasses on the model itself
/// ([`Model::sti_subclass_names`]) — the fact `push_dom_prefix_method`
/// needs to make the base's `dom_prefix` a type-column dispatch, and
/// which only this pass derives (subclasses are library classes, not
/// Models, so the per-model synthesizer can't see them). Sorted, so
/// the generated `case` arms are deterministic.
fn stamp_sti_subclasses(app: &mut App, bases: &HashMap<ClassId, ClassId>) {
    let mut by_base: HashMap<ClassId, Vec<ClassId>> = HashMap::default();
    for (subclass, base) in bases {
        by_base.entry(base.clone()).or_default().push(subclass.clone());
    }
    for (base, mut subs) in by_base {
        subs.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        if let Some(model) = app.models.iter_mut().find(|m| m.name == base) {
            model.sti_subclass_names = subs;
        }
    }
}

/// Give every recast target a `self.becomes_from(source)`:
///
/// ```text
/// def self.becomes_from(source)
///   record = Rooms::Open.new
///   record[:created_at] = source[:created_at]
///   record[:creator_id] = source[:creator_id]
///   record[:name] = source[:name]
///   record[:type] = "Rooms::Open"          # the stamp
///   record[:updated_at] = source[:updated_at]
///   record.id = source.id
///   record.mark_persisted! if source.persisted?
///   record
/// end
/// ```
///
/// Columns come from the BASE model's table — an STI subclass declares
/// none of its own — and are copied through `[]`/`[]=` rather than the
/// named accessors, because the column-to-slot mapping is the model's
/// own (a timestamp column reaches `@created_at_raw`) and the indexer
/// is where each model already writes that mapping down.
fn push_becomes_from(app: &mut App, bases: &HashMap<ClassId, ClassId>, recast: &HashSet<ClassId>) {
    let named = runtime_sti_named(app);
    // Column list per base model, read before the mutable borrow below.
    let columns: HashMap<ClassId, Vec<Symbol>> = app
        .models
        .iter()
        .map(|m| {
            let cols = app
                .schema
                .tables
                .get(&m.table.0)
                .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
                .unwrap_or_default();
            (m.name.clone(), cols)
        })
        .collect();

    let sti_cols = sti_columns(app);
    for lc in &mut app.library_classes {
        if !recast.contains(&lc.name) {
            continue;
        }
        let Some(base) = bases.get(&lc.name) else { continue };
        if lc.methods.iter().any(|m| m.name.as_str() == BECOMES_FROM) {
            continue;
        }
        let source = Symbol::from("source");
        let record = Symbol::from("record");
        let sub_ty = Ty::Class { id: lc.name.clone(), args: vec![].into() };
        let mut body: Vec<Expr> = vec![Expr::new(
            Span::synthetic(),
            ExprNode::Assign {
                target: LValue::Var { id: VarId(0), name: record.clone() },
                value: Expr::new(
                    Span::synthetic(),
                    ExprNode::Send {
                        recv: Some(class_const(&lc.name)),
                        method: Symbol::from("new"),
                        args: vec![],
                        block: None,
                        parenthesized: true,
                    },
                ),
            },
        )];
        // `record[:col] = source[:col]` per column — the indexer, not
        // the named accessor, because the column-to-slot mapping is the
        // model's own (a timestamp column reaches `@created_at_raw`)
        // and `[]`/`[]=` are where each model already writes it down.
        for col in columns.get(base).map(|v| v.as_slice()).unwrap_or_default() {
            // The primary key rides its own accessor below — `id` is on
            // Base whether or not the schema lists it as a column, so
            // writing it here as well would write it twice.
            if col.as_str() == "id" {
                continue;
            }
            let value = if col.as_str() == column_of(&sti_cols, base) {
                // The stamp: a recast row belongs to the new subclass.
                if named.contains(&lc.name) {
                    sti_name_call(Span::synthetic(), lc.name.0.as_str())
                } else {
                    Expr::new(
                        Span::synthetic(),
                        ExprNode::Lit {
                            value: Literal::Str { value: lc.name.0.as_str().to_string() },
                        },
                    )
                }
            } else {
                index_read(var_ref(&source), col)
            };
            body.push(Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(var_ref(&record)),
                    method: Symbol::from("[]="),
                    args: vec![sym_lit(col), value],
                    block: None,
                    parenthesized: true,
                },
            ));
        }
        // The id is the whole point of a recast — the sibling IS this
        // row — and `attr_accessor :id` on Base is the one writer every
        // model has, schema listing or not.
        body.push(Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(var_ref(&record)),
                method: Symbol::from("id="),
                args: vec![no_arg_send(var_ref(&source), "id")],
                block: None,
                parenthesized: true,
            },
        ));
        // Carry the persisted flag: a recast row is the SAME row, so the
        // save after it must UPDATE. Without this the next save inserts
        // a duplicate.
        body.push(Expr::new(
            Span::synthetic(),
            ExprNode::If {
                cond: no_arg_send(var_ref(&source), "persisted?"),
                then_branch: no_arg_send(var_ref(&record), "mark_persisted!"),
                else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            },
        ));
        body.push(var_ref(&record));
        lc.methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: Symbol::from(BECOMES_FROM),
            receiver: MethodReceiver::Class,
            params: vec![Param::positional(source.clone())],
            body: Expr::new(Span::synthetic(), ExprNode::Seq { exprs: body }),
            signature: Some(crate::lower::typing::fn_sig(
                vec![(source, Ty::Class { id: base.clone(), args: vec![].into() })],
                sub_ty,
            )),
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(lc.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

/// `all` / `where(…)` / … with no receiver (or `self`) in a subclass's
/// class method → the same call on the subclass constant. Only the
/// ROOT of a chain has no receiver, so the walk reaches it through the
/// receivers above; block bodies are walked too (a nested query in a
/// `detect` block is still the class's own).
fn name_implicit_receiver(expr: &mut Expr, sub: &ClassId) {
    expr.node.for_each_child_mut(&mut |c| name_implicit_receiver(c, sub));
    let ExprNode::Send { recv, method, .. } = &mut *expr.node else { return };
    if !STI_RELATION_METHODS.contains(&method.as_str()) {
        return;
    }
    let implicit = match recv {
        None => true,
        Some(r) => matches!(&*r.node, ExprNode::SelfRef),
    };
    if implicit {
        *recv = Some(class_const(sub));
    }
}

fn class_const(id: &ClassId) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Const { path: id.0.as_str().split("::").map(Symbol::from).collect() },
    )
}

fn var_ref(name: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Var { id: VarId(0), name: name.clone() })
}

fn sym_lit(name: &Symbol) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Sym { value: name.clone() } })
}

fn index_read(recv: Expr, col: &Symbol) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from("[]"),
            args: vec![sym_lit(col)],
            block: None,
            parenthesized: true,
        },
    )
}

fn no_arg_send(recv: Expr, method: &str) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from(method),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    )
}

/// STI subclass → its base model. A subclass is a library class whose
/// `parent` chain reaches a Model, and that Model's table has to carry
/// the inheritance column — without it there is nothing to filter on
/// and the class is plain Ruby inheritance, not STI.
/// Public because the Ruby-family hydration pass
/// (`emit::ruby::library::apply_sti_hydration`) has to ask the SAME
/// question this file already answers — which classes are STI
/// subclasses of which base — rather than keeping a second copy of the
/// parent walk and the inheritance-column check that would drift from
/// this one.
pub(crate) fn sti_bases(app: &App) -> HashMap<ClassId, ClassId> {
    let mut out = HashMap::default();
    // Name -> the FIRST model / library class of that name, the same
    // element the linear `find`s this replaced answered. Indexed once:
    // a scan per parent hop is |classes|^2 on full Core.
    let mut models_by_name: HashMap<&ClassId, &crate::dialect::Model> = HashMap::default();
    for m in &app.models {
        models_by_name.entry(&m.name).or_insert(m);
    }
    let mut lc_parent: HashMap<&ClassId, &Option<ClassId>> = HashMap::default();
    for other in &app.library_classes {
        lc_parent.entry(&other.name).or_insert(&other.parent);
    }
    let model_named = |id: &ClassId| models_by_name.get(id).copied();
    for lc in &app.library_classes {
        let mut cursor = lc.parent.clone();
        let mut hops = 0;
        while let Some(parent) = cursor {
            if hops > 8 {
                break;
            }
            if let Some(base) = model_named(&parent) {
                let column = inheritance_column(base, &app.models);
                let has_type = !column.is_empty()
                    && app
                        .schema
                        .tables
                        .get(&base.table.0)
                        .is_some_and(|t| t.columns.iter().any(|c| c.name.as_str() == column));
                if has_type {
                    out.insert(lc.name.clone(), base.name.clone());
                }
                break;
            }
            cursor = lc_parent.get(&parent).and_then(|p| (*p).clone());
            hops += 1;
        }
    }
    out
}

/// The STI classes (subclasses and their base models) whose stored type
/// is the app's own `sti_name` rather than the class's full name. Rails
/// writes and filters `sti_name` (`type_condition` maps `([self] +
/// descendants)` through it) and instantiates through `find_sti_class`,
/// which asks `sti_class_for`. An app overrides them on the base or a
/// subclass (Core's `CustomerOauthApplication.sti_name` is
/// `name.demodulize`), extends a module that defines `sti_name`
/// (`StiLookupTable`), or sets `store_full_sti_class` /
/// `store_full_class_name`. Such a hierarchy carries `<Class>.sti_name`
/// where the others carry the name literal, which every target can
/// compare, and the Ruby family hydrates it through the runtime
/// `find_sti_class`. Apps without such a hierarchy are left
/// byte-identical.
pub(crate) fn runtime_sti_named(app: &App) -> HashSet<ClassId> {
    let bases = sti_bases(app);
    let mut out: HashSet<ClassId> = HashSet::default();
    if bases.is_empty() {
        return out;
    }
    const OVERRIDES: [&str; 3] = ["sti_name", "sti_class_for", "find_sti_class"];
    const SETTINGS: [&str; 2] = ["store_full_sti_class=", "store_full_class_name="];
    let modules: Vec<&crate::dialect::LibraryClass> = app.library_classes.iter().filter(|lc| lc.is_module).collect();
    let module_named = |name: &str| {
        modules.iter().copied().find(|m| {
            let full = m.name.0.as_str();
            full == name || full.ends_with(&format!("::{name}"))
        })
    };
    // A body call that sets a naming option, or extends a module that
    // defines `sti_name` / `sti_class_for` or sets one.
    let renames = |call: &Expr| -> bool {
        let mut found = false;
        visit_sends(call, &mut |method, args| {
            if SETTINGS.contains(&method) {
                found = true;
            } else if method == "extend" {
                for a in args {
                    let ExprNode::Const { path } = &*a.node else { continue };
                    let Some(m) = module_named(const_class_id(path).0.as_str()) else { continue };
                    let defines = m.methods.iter().any(|d| OVERRIDES.contains(&d.name.as_str()));
                    let sets = m.methods.iter().any(|d| {
                        let mut s = false;
                        visit_sends(&d.body, &mut |n, _| s = s || SETTINGS.contains(&n));
                        s
                    });
                    found = found || defines || sets;
                }
            }
        });
        found
    };
    let class_override = |m: &MethodDef| m.receiver == MethodReceiver::Class && OVERRIDES.contains(&m.name.as_str());
    let mut parent_of: HashMap<&ClassId, &ClassId> = HashMap::default();
    let mut own: HashMap<&ClassId, bool> = HashMap::default();
    for lc in &app.library_classes {
        if let Some(p) = &lc.parent {
            parent_of.entry(&lc.name).or_insert(p);
        }
        let named = lc.methods.iter().any(class_override)
            || lc.unknown_calls.iter().any(renames)
            || lc.class_ivar_initializers.iter().any(renames);
        *own.entry(&lc.name).or_insert(false) |= named;
    }
    for m in &app.models {
        if let Some(p) = &m.parent {
            parent_of.entry(&m.name).or_insert(p);
        }
        let named = m.methods().any(class_override)
            || m.body.iter().any(|item| match item {
                crate::dialect::ModelBodyItem::Unknown { expr, .. }
                | crate::dialect::ModelBodyItem::ClassBody { expr, .. } => renames(expr),
                _ => false,
            });
        *own.entry(&m.name).or_insert(false) |= named;
    }
    // A hierarchy is its base model, the base's ancestors and every
    // subclass; any one of them renaming makes them all runtime-named.
    let mut renamed_bases: HashSet<&ClassId> = HashSet::default();
    for (sub, base) in &bases {
        let mut chain = vec![sub];
        let mut cursor = parent_of.get(sub).copied();
        for _ in 0..12 {
            let Some(p) = cursor else { break };
            chain.push(p);
            cursor = parent_of.get(p).copied();
        }
        if chain.iter().any(|c| own.get(*c).copied().unwrap_or(false)) {
            renamed_bases.insert(base);
        }
    }
    for (sub, base) in &bases {
        if renamed_bases.contains(base) {
            out.insert(sub.clone());
            out.insert(base.clone());
        }
    }
    out
}

/// Each `Send` in `expr`: its method name and arguments.
fn visit_sends(expr: &Expr, f: &mut dyn FnMut(&str, &[Expr])) {
    if let ExprNode::Send { method, args, .. } = &*expr.node {
        f(method.as_str(), args);
    }
    expr.node.for_each_child(&mut |c| visit_sends(c, f));
}

/// `<Class>.sti_name`, the runtime type value of a runtime-named class.
pub(crate) fn sti_name_call(span: Span, class: &str) -> Expr {
    Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(span, ExprNode::Const { path: class.split("::").map(Symbol::from).collect() })),
            method: Symbol::from("sti_name"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    )
}

/// The class names a type value carries: a name literal or a runtime
/// `<Class>.sti_name`.
fn type_value_name(e: &Expr) -> Option<String> {
    match &*e.node {
        ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
        ExprNode::Send { recv: Some(r), method, args, .. } if method.as_str() == "sti_name" && args.is_empty() => {
            match &*r.node {
                ExprNode::Const { path } => Some(const_class_id(path).0.as_str().to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Each STI subclass's `type` values: its own name, then every STI
/// class below it (`GoldBadge` -> `GoldBadge`, `PlatinumBadge`), the
/// `type IN (...)` Rails' `type_condition` writes from `descendants`.
fn type_names(app: &App, bases: &HashMap<ClassId, ClassId>) -> HashMap<ClassId, Vec<String>> {
    let parent_of: HashMap<&ClassId, &ClassId> = app
        .library_classes
        .iter()
        .filter_map(|lc| lc.parent.as_ref().map(|p| (&lc.name, p)))
        .collect();
    let mut out: HashMap<ClassId, Vec<String>> = HashMap::default();
    for lc in &app.library_classes {
        if !bases.contains_key(&lc.name) || out.contains_key(&lc.name) {
            continue;
        }
        let mut names = vec![lc.name.0.as_str().to_string()];
        for other in &app.library_classes {
            if other.name == lc.name || !bases.contains_key(&other.name) {
                continue;
            }
            let mut cursor = parent_of.get(&other.name).copied();
            for _ in 0..8 {
                let Some(parent) = cursor else { break };
                if parent == &lc.name {
                    let name = other.name.0.as_str().to_string();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                    break;
                }
                cursor = parent_of.get(parent).copied();
            }
        }
        out.insert(lc.name.clone(), names);
    }
    out
}

/// `bases` maps each STI subclass to the class its scoped relation roots
/// on: its base model, or itself when it declares attribute types of its
/// own (`apply_sti_scope_lowering`).
fn rewrite(
    expr: &mut Expr,
    bases: &HashMap<ClassId, ClassId>,
    types: &HashMap<ClassId, Vec<String>>,
    named: &HashSet<ClassId>,
    cols: &HashMap<ClassId, Symbol>,
    recast: &mut HashSet<ClassId>,
) {
    expr.node.for_each_child_mut(&mut |c| rewrite(c, bases, types, named, cols, recast));
    if rewrite_becomes(expr, bases, recast) {
        return;
    }
    let ExprNode::Send { recv: Some(r), method, args, block, .. } = &*expr.node else { return };
    let constructing = STI_CONSTRUCTORS.contains(&method.as_str());
    // `Sub.unscoped` drops the default scopes but keeps the type
    // condition (Rails' `relation`): `Root.unscoped.where(type: "Sub")`.
    let unscoped = method.as_str() == "unscoped" && args.is_empty() && block.is_none();
    if !constructing && !unscoped && !STI_RELATION_METHODS.contains(&method.as_str()) {
        return;
    }
    let ExprNode::Const { path } = &*r.node else { return };
    // A root rewritten from an implicit receiver (`name_implicit_receiver`)
    // stays synthetic, so the Ruby family can seed it on `self` instead
    // (`unseat_implicit_roots`).
    let implicit = r.span.is_synthetic();
    let class = ClassId(Symbol::from(
        path.iter()
            .map(|s| s.as_str().trim_start_matches("::"))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("::"),
    ));
    let Some(_) = bases.get(&class) else { return };
    let column = column_of(cols, &class).to_string();
    if constructing {
        stamp_type(expr, &class, &column, named.contains(&class));
        return;
    }
    let base = bases.get(&class).expect("checked above");
    let span = expr.span;
    let root_span = if implicit { Span::synthetic() } else { span };
    let mut root = Expr::new(root_span, ExprNode::Const { path: base.0.as_str().split("::").map(Symbol::from).collect() });
    if unscoped {
        root = Expr::new(
            span,
            ExprNode::Send { recv: Some(root), method: Symbol::from("unscoped"), args: vec![], block: None, parenthesized: false },
        );
    }
    let scoped = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(root),
            method: Symbol::from("where"),
            args: vec![Expr::new(
                span,
                ExprNode::Hash {
                    entries: vec![(
                        Expr::new(
                            span,
                            ExprNode::Lit {
                                value: Literal::Sym { value: Symbol::from(column.as_str()) },
                            },
                        ),
                        type_value(span, &class, types, named),
                    )],
                    kwargs: true,
                },
            )],
            block: None,
            parenthesized: true,
        },
    );
    // `.all` IS the scope, so the call disappears into it. Everything
    // else rides the scoped relation.
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { method, args, block, parenthesized, .. } = node else { unreachable!() };
    if (unscoped || method.as_str() == "all") && args.is_empty() && block.is_none() {
        *expr = scoped;
        return;
    }
    *expr = Expr::new(
        span,
        ExprNode::Send { recv: Some(scoped), method, args, block, parenthesized },
    );
}

/// Ruby family: each STI subclass's `_apply_type_condition(relation)`,
/// `relation.where!(type: ["Dog", "Puppy"])`, which the runtime
/// `Relation#initialize` applies to every relation on the class (Rails'
/// `type_condition`). A relation seeded on `self` in a class method
/// then reads the receiving class's rows: `Animal.recent` reached as
/// `Dog.recent`, or `Dog.loud` inherited by `Puppy`. Apps without STI
/// are left byte-identical.
pub(crate) fn push_type_condition_methods(lcs: &mut [crate::dialect::LibraryClass], app: &App) {
    let bases = sti_bases(app);
    if bases.is_empty() {
        return;
    }
    let types = type_names(app, &bases);
    let named = runtime_sti_named(app);
    let cols = sti_columns(app);
    let relation = Symbol::from("relation");
    for lc in lcs.iter_mut() {
        if !bases.contains_key(&lc.name)
            || lc.methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == TYPE_CONDITION)
        {
            continue;
        }
        let span = Span::synthetic();
        let column = column_of(&cols, &lc.name).to_string();
        let condition = Expr::new(
            span,
            ExprNode::Hash {
                entries: vec![(
                    Expr::new(span, ExprNode::Lit { value: Literal::Sym { value: Symbol::from(column.as_str()) } }),
                    type_value(span, &lc.name, &types, &named),
                )],
                kwargs: true,
            },
        );
        let body = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(Expr::new(span, ExprNode::Var { id: VarId(0), name: relation.clone() })),
                // In place: `Relation#initialize` does not read the value.
                method: Symbol::from("where!"),
                args: vec![condition],
                block: None,
                parenthesized: true,
            },
        );
        // Rails reads `Sub.find(id)` through the relation too, so the
        // runtime finders take the relation path for the class.
        if !lc.methods.iter().any(|m| m.receiver == MethodReceiver::Class && m.name.as_str() == TYPE_CONDITIONED) {
            lc.methods.push(MethodDef {
                visibility: crate::dialect::MethodVisibility::Public,
                unsupported_formals: None,
                has_anonymous_block: false,
                name_span: Span::synthetic(),
                name: Symbol::from(TYPE_CONDITIONED),
                receiver: MethodReceiver::Class,
                params: vec![],
                body: Expr::new(span, ExprNode::Lit { value: Literal::Bool { value: true } }),
                signature: None,
                effects: crate::effect::EffectSet::default(),
                enclosing_class: Some(lc.name.0.clone()),
                kind: AccessorKind::Method,
                is_async: false,
                mutates_self: false,
                block_param: None,
            });
        }
        lc.methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: Span::synthetic(),
            name: Symbol::from(TYPE_CONDITION),
            receiver: MethodReceiver::Class,
            params: vec![Param::positional(relation.clone())],
            body,
            signature: None,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(lc.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}

const TYPE_CONDITION: &str = "_apply_type_condition";
const TYPE_CONDITIONED: &str = "_type_conditioned?";

/// Ruby family: in an STI subclass's own class methods, the implicit
/// receiver `apply_sti_scope_lowering` rooted on the base with the
/// subclass's static type list (`where(sound: "woof")` ->
/// `Animal.where(type: ["Dog", "Puppy"]).where(sound: "woof")`) goes
/// back to the implicit form, which `apply_scope_lowering` seeds on
/// `self`; the runtime type condition (`push_type_condition_methods`)
/// scopes it to the receiving class, so `Puppy` inheriting `Dog.loud`
/// reads Puppies. An author-written constant keeps its root.
pub(crate) fn unseat_implicit_roots(lcs: &mut [crate::dialect::LibraryClass], app: &App) {
    let bases = sti_bases(app);
    if bases.is_empty() {
        return;
    }
    let types = type_names(app, &bases);
    let cols = sti_columns(app);
    for lc in lcs.iter_mut() {
        let Some(base) = bases.get(&lc.name) else { continue };
        let names = types.get(&lc.name).cloned().unwrap_or_else(|| vec![lc.name.0.as_str().to_string()]);
        let column = column_of(&cols, &lc.name).to_string();
        let root = ImplicitRoot { base, column: &column, names: &names };
        for m in &mut lc.methods {
            if m.receiver == MethodReceiver::Class && !m.name_span.is_synthetic() {
                unseat(&mut m.body, &root);
            }
        }
    }
}

struct ImplicitRoot<'a> {
    base: &'a ClassId,
    column: &'a str,
    names: &'a [String],
}

fn unseat(expr: &mut Expr, root: &ImplicitRoot) {
    if is_implicit_root(expr, root) {
        *expr = Expr::new(
            expr.span,
            ExprNode::Send { recv: None, method: Symbol::from("all"), args: vec![], block: None, parenthesized: false },
        );
        return;
    }
    // `Base.where(type: …).order(:id)` was `order(:id)`, a chain start
    // `apply_scope_lowering` seeds bare (as it does `first(2)`); any
    // other call (`all.map`, `count`) keeps `all` as its receiver.
    if let ExprNode::Send { recv, method, args, block, .. } = &mut *expr.node {
        let seeded_bare = matches!(
            method.as_str(),
            "where" | "order" | "limit" | "offset" | "joins" | "includes" | "distinct" | "group"
        ) || (matches!(method.as_str(), "first" | "last") && args.len() == 1 && block.is_none());
        if seeded_bare && recv.as_ref().is_some_and(|r| is_implicit_root(r, root)) {
            *recv = None;
        }
    }
    expr.node.for_each_child_mut(&mut |c| unseat(c, root));
}

/// `Base.where(type: <the subclass's type value>)` on a synthetic `Base`.
fn is_implicit_root(expr: &Expr, root: &ImplicitRoot) -> bool {
    let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &*expr.node else { return false };
    if method.as_str() != "where" || args.len() != 1 || !r.span.is_synthetic() {
        return false;
    }
    let ExprNode::Const { path } = &*r.node else { return false };
    if const_class_id(path) != *root.base {
        return false;
    }
    let ExprNode::Hash { entries, .. } = &*args[0].node else { return false };
    let [(key, value)] = entries.as_slice() else { return false };
    let key_ok = matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == root.column);
    let values: Option<Vec<String>> = match &*value.node {
        ExprNode::Array { elements, .. } if root.names.len() > 1 => elements.iter().map(type_value_name).collect(),
        _ if root.names.len() == 1 => type_value_name(value).map(|v| vec![v]),
        _ => None,
    };
    key_ok && values.as_deref() == Some(root.names)
}

/// Ruby family: the class methods of a model with subclasses (STI or
/// plain) that read its implicit `all` (`def self.everything; all.to_a;
/// end`, which ingest spells `Asset.all`). Rails reads that relation on
/// the receiving class, so `BigAsset.everything` loads BigAssets; the
/// arel fold would bake in the model's whole-table read, so it is held
/// back for these and `apply_scope_lowering` seeds them on `self`.
pub(crate) fn subclassed_self_reads(app: &App) -> HashSet<(ClassId, Symbol)> {
    let parents: HashSet<ClassId> =
        sti_bases(app).into_values().chain(plain_model_subclasses(app).into_values()).collect();
    let mut out = HashSet::default();
    for model in app.models.iter().filter(|m| parents.contains(&m.name)) {
        for m in model.methods() {
            if m.receiver == MethodReceiver::Class && reads_implicit_all(&m.body, &model.name) {
                out.insert((model.name.clone(), m.name.clone()));
            }
        }
    }
    out
}

fn reads_implicit_all(expr: &Expr, model: &ClassId) -> bool {
    if let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node {
        let implicit = match recv {
            None => true,
            Some(r) => match &*r.node {
                ExprNode::SelfRef => true,
                ExprNode::Const { path } => {
                    (r.span.is_synthetic() || r.span == expr.span) && const_class_id(path) == *model
                }
                _ => false,
            },
        };
        if implicit && method.as_str() == "all" && args.is_empty() {
            return true;
        }
    }
    let mut found = false;
    expr.node.for_each_child(&mut |c| found = found || reads_implicit_all(c, model));
    found
}

/// `"Rooms::Open"`, or `["GoldBadge", "PlatinumBadge"]` when STI
/// classes sit below the subclass; `Rooms::Open.sti_name` for each
/// class of a runtime-named hierarchy ([`runtime_sti_named`]).
fn type_value(span: Span, class: &ClassId, types: &HashMap<ClassId, Vec<String>>, named: &HashSet<ClassId>) -> Expr {
    let value_of = |value: &str| {
        if named.contains(&ClassId(Symbol::from(value))) {
            sti_name_call(span, value)
        } else {
            Expr::new(span, ExprNode::Lit { value: Literal::Str { value: value.to_string() } })
        }
    };
    match types.get(class) {
        Some(names) if names.len() > 1 => Expr::new(
            span,
            ExprNode::Array { elements: names.iter().map(|n| value_of(n)).collect(), style: Default::default() },
        ),
        _ => value_of(class.0.as_str()),
    }
}

/// `room.becomes!(Rooms::Open)` -> `Rooms::Open.becomes_from(room)`.
///
/// Only `becomes!` — Rails' non-bang `becomes` leaves the inheritance
/// column alone, which is a different operation, and no corpus app
/// writes it. The receiver moves into an argument position, so it is
/// evaluated exactly once either way and any expression is safe there.
///
/// Returns true when it rewrote, so the caller skips the relation/
/// constructor paths below (they key off a Const RECEIVER; here the
/// const is the ARGUMENT).
fn rewrite_becomes(
    expr: &mut Expr,
    bases: &HashMap<ClassId, ClassId>,
    recast: &mut HashSet<ClassId>,
) -> bool {
    let ExprNode::Send { method, args, block, .. } = &*expr.node else {
        return false;
    };
    if method.as_str() != "becomes!" || args.len() != 1 || block.is_some() {
        return false;
    }
    let ExprNode::Const { path } = &*args[0].node else { return false };
    let named = const_class_id(path);
    if !bases.contains_key(&named) {
        return false;
    }
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { recv, .. } = node else { unreachable!() };
    // Implicit self — campfire's `Room#destroy_later` writes
    // `becomes!(Rooms::Closed)` on the record itself.
    let source = recv.unwrap_or_else(|| Expr::new(span, ExprNode::SelfRef));
    recast.insert(named.clone());
    *expr = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(class_const(&named)),
            method: Symbol::from(BECOMES_FROM),
            args: vec![source],
            block: None,
            parenthesized: true,
        },
    );
    true
}

/// A `Const { path }` as the fully-qualified class name it names.
/// Leading `::` is trimmed per segment — the ingest keeps it on an
/// explicitly-rooted constant.
fn const_class_id(path: &[Symbol]) -> ClassId {
    ClassId(Symbol::from(
        path.iter()
            .map(|s| s.as_str().trim_start_matches("::"))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("::"),
    ))
}

/// Fold `type: "<Sub>"` into a constructor's attribute hash.
///
/// The RECEIVER stays the subclass: Ruby's inheritance constructs a
/// `Rooms::Open`, which is what campfire's `Room#open?` (`is_a?
/// (Rooms::Open)`) asks about — rerooting on the base would answer that
/// wrongly while fixing the column. Only the column is missing.
///
/// An existing `type:` wins; an argument that is not a literal Hash
/// (a params object, a variable) is left alone, because there is no
/// literal to fold into and building a `merge` here would hand the
/// constructor a shape it does not take.
fn stamp_type(expr: &mut Expr, sub: &ClassId, column: &str, runtime_named: bool) {
    let span = expr.span;
    let ExprNode::Send { args, .. } = &mut *expr.node else { return };
    let Some(attrs) = args.last_mut() else { return };
    let ExprNode::Hash { entries, .. } = &mut *attrs.node else { return };
    let already = entries.iter().any(|(k, _)| {
        matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } }
            if value.as_str() == column)
    });
    if already {
        return;
    }
    entries.push((
        Expr::new(
            span,
            ExprNode::Lit { value: Literal::Sym { value: Symbol::from(column) } },
        ),
        if runtime_named {
            sti_name_call(span, sub.0.as_str())
        } else {
            Expr::new(span, ExprNode::Lit { value: Literal::Str { value: sub.0.as_str().to_string() } })
        },
    ));
}
