//! A model's transaction callbacks (`after_commit` and its
//! `after_{create,update,destroy,save}_commit` sugar, `after_rollback`,
//! `before_commit`) → the three callback chains Rails builds for them,
//! each emitted as one method the runtime runs when the record's
//! transaction commits or rolls back:
//!
//! ```text
//! _run_commit_callbacks        # :commit, after
//! _run_rollback_callbacks      # :rollback, after
//! _run_before_commit_callbacks # :before_commit, before
//! ```
//!
//! The chains are built at compile time with the rules the native run
//! uses (Shopify rails 5813559f4aed, 8.2.0.alpha: activerecord
//! transactions.rb, activesupport callbacks.rb):
//!
//! - an ancestor model's declarations come first (a subclass's chain
//!   starts as a copy of its parent's);
//! - a symbol callback removes an earlier one with the same name from
//!   the same chain (`CallbackChain#remove_duplicates`), so
//!   `after_create_commit :f; after_update_commit :f` keeps only the
//!   update one;
//! - `prepend: true` puts the callback at the front, and
//!   `ActiveRecord.run_after_transaction_callbacks_in_order_defined`
//!   flips `prepend` for the two after chains (`enforced_prepend_option`;
//!   [`Model::after_transaction_callbacks_in_order`]);
//! - an after chain runs from its last callback to its first, the
//!   before chain in order;
//! - `on:` is the first condition, `transaction_include_any_action?`
//!   (the runtime's `_transaction_include_*?`), then every `if:`, then
//!   every negated `unless:`. Other keys (Core's `only:`) are ignored
//!   by ActiveSupport::Callbacks and here.
//!
//! A callback calls its method (`foo`), its callback object
//! (`object.after_commit(self)`; one object per declaration, built on
//! first use rather than at class load), or its block, which becomes a
//! method of the declaring model (a one-parameter block is
//! `instance_exec(record)`: its parameter is the record, as `self` is).
//!
//! The runtime half is `runtime/ruby/active_record/base.rb` (the write
//! paths and the predicates) and, on the CRuby PostgreSQL tree,
//! `runtime/spinel/cruby/active_record_transactions.rb` (record
//! enrollment: the callbacks run after the outermost commit, the
//! rollback chain on rollback). Checked against native Rails in
//! tests/transaction_callbacks_pg_runtime.rs.

use crate::dialect::{
    AccessorKind, CallbackHook, CallbackOn, MethodDef, MethodReceiver, MethodVisibility, Model,
    ModelBodyItem,
};
use crate::effect::EffectSet;
use crate::expr::{BoolOpKind, BoolOpSurface, Expr, ExprNode, LValue, Literal, OpAssignOp};
use crate::ident::Symbol;
use crate::span::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chain {
    Commit,
    Rollback,
    BeforeCommit,
}

impl Chain {
    const ALL: [Chain; 3] = [Chain::Commit, Chain::Rollback, Chain::BeforeCommit];

    fn method(self) -> &'static str {
        match self {
            Chain::Commit => "_run_commit_callbacks",
            Chain::Rollback => "_run_rollback_callbacks",
            Chain::BeforeCommit => "_run_before_commit_callbacks",
        }
    }

    /// The method ActiveSupport::Callbacks' `ObjectCall` sends a callback
    /// object: the chain's `scope: [:kind, :name]` joined.
    fn object_method(self) -> &'static str {
        match self {
            Chain::Commit => "after_commit",
            Chain::Rollback => "after_rollback",
            Chain::BeforeCommit => "before_before_commit",
        }
    }

    fn is_after(self) -> bool {
        !matches!(self, Chain::BeforeCommit)
    }
}

/// The transaction-callback declaration names, with the `on:` each
/// enforces (`set_options_for_callbacks!`'s `enforced_options`).
fn hook(name: &str) -> Option<(Chain, &'static [CallbackOn])> {
    use CallbackOn::*;
    Some(match name {
        "after_commit" => (Chain::Commit, &[]),
        "after_create_commit" => (Chain::Commit, &[Create]),
        "after_update_commit" => (Chain::Commit, &[Update]),
        "after_destroy_commit" => (Chain::Commit, &[Destroy]),
        "after_save_commit" => (Chain::Commit, &[Create, Update]),
        "after_rollback" => (Chain::Rollback, &[]),
        "before_commit" => (Chain::BeforeCommit, &[]),
        _ => return None,
    })
}

/// Is `name` a transaction-callback declaration? Its block forms are
/// lowered here, not by `markers::push_block_callback`.
pub(crate) fn is_transaction_hook(name: &str) -> bool {
    hook(name).is_some()
}

/// One callback in a chain.
struct Entry {
    chain: Chain,
    /// The symbol filter, the only kind `remove_duplicates` matches.
    dedupe: Option<Symbol>,
    /// The `on:` guard and conditions, all of which must hold.
    guard: Vec<Expr>,
    call: Expr,
    prepend: bool,
    span: Span,
}

/// What a block-form declaration contributes besides its entry: the
/// method its block becomes.
struct BlockForm {
    entry: Entry,
    method: MethodDef,
}

/// The transaction-callback methods of `model`: the chains it extends
/// (an ancestor model's chain comes first), the methods its blocks
/// became, and the memoized callback objects.
pub(super) fn push_transaction_callback_methods(methods: &mut Vec<MethodDef>, model: &Model, models: &[Model]) {
    let mut own: Vec<Entry> = Vec::new();
    for (index, item) in model.body.iter().enumerate() {
        match declaration(model, index, item) {
            Some(Declared::Entries(entries, extra)) => {
                own.extend(entries);
                methods.extend(extra);
            }
            Some(Declared::Block(block)) => {
                own.push(block.entry);
                methods.push(block.method);
            }
            None => {}
        }
    }
    if own.is_empty() {
        return;
    }
    let mut lineage: Vec<&Model> = Vec::new();
    let mut parent = model.parent.as_ref();
    while let Some(ancestor) = parent.and_then(|p| models.iter().find(|m| &m.name == p)) {
        if lineage.iter().any(|m| m.name == ancestor.name) || ancestor.name == model.name {
            break;
        }
        lineage.push(ancestor);
        parent = ancestor.parent.as_ref();
    }
    let mut all: Vec<Entry> = Vec::new();
    for ancestor in lineage.iter().rev() {
        for (index, item) in ancestor.body.iter().enumerate() {
            match declaration(ancestor, index, item) {
                Some(Declared::Entries(entries, _)) => all.extend(entries),
                Some(Declared::Block(block)) => all.push(block.entry),
                None => {}
            }
        }
    }
    let own_chains: Vec<Chain> = Chain::ALL.into_iter().filter(|c| own.iter().any(|e| e.chain == *c)).collect();
    all.extend(own);
    for chain in own_chains {
        let mut built: Vec<Entry> = Vec::new();
        for entry in all.iter().filter(|e| e.chain == chain) {
            if let Some(name) = &entry.dedupe {
                built.retain(|e| e.dedupe.as_ref() != Some(name));
            }
            let prepend = if chain.is_after() && model.after_transaction_callbacks_in_order {
                !entry.prepend
            } else {
                entry.prepend
            };
            let copy = Entry {
                chain,
                dedupe: entry.dedupe.clone(),
                guard: entry.guard.clone(),
                call: entry.call.clone(),
                prepend: entry.prepend,
                span: entry.span,
            };
            if prepend {
                built.insert(0, copy);
            } else {
                built.push(copy);
            }
        }
        if chain.is_after() {
            built.reverse();
        }
        let stmts: Vec<Expr> = built.into_iter().map(statement).collect();
        let body = match stmts.len() {
            1 => stmts.into_iter().next().unwrap(),
            _ => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: stmts }),
        };
        methods.retain(|m| !(m.name.as_str() == chain.method() && m.receiver == MethodReceiver::Instance));
        methods.push(method_def(model, chain.method(), MethodReceiver::Instance, body, Some(void())));
    }
}

/// Does the lowering here claim this Unknown class-body call? Every
/// transaction-callback declaration it lowers; any other (an option it
/// does not read, a block it cannot splice) stays reported.
pub(crate) fn claims(model: &Model, expr: &Expr) -> bool {
    block_form(model, 0, expr).is_some()
}

enum Declared {
    Entries(Vec<Entry>, Vec<MethodDef>),
    Block(BlockForm),
}

fn declaration(model: &Model, index: usize, item: &ModelBodyItem) -> Option<Declared> {
    match item {
        ModelBodyItem::Callback { callback, span, .. } if callback.hook.is_transaction() => {
            let chain = match callback.hook {
                CallbackHook::AfterCommit | CallbackHook::AfterSaveCommit => Chain::Commit,
                CallbackHook::AfterRollback => Chain::Rollback,
                CallbackHook::BeforeCommit => Chain::BeforeCommit,
                _ => return None,
            };
            let span = *span;
            let mut guard: Vec<Expr> = Vec::new();
            if let Some(on) = on_guard(&callback.on_any, span) {
                guard.push(on);
            }
            if let Some(cond) = &callback.condition {
                guard.push(cond.clone());
            }
            let mut entries = Vec::new();
            let mut extra = Vec::new();
            for target in &callback.targets {
                entries.push(Entry {
                    chain,
                    dedupe: Some(target.clone()),
                    guard: guard.clone(),
                    call: self_send(target.as_str(), Vec::new(), span),
                    prepend: callback.prepend,
                    span,
                });
            }
            if let Some(object) = &callback.object {
                let (call, memo) = object_call(model, index, object, chain, span);
                entries.push(Entry { chain, dedupe: None, guard, call, prepend: callback.prepend, span });
                extra.extend(memo);
            }
            Some(Declared::Entries(entries, extra))
        }
        ModelBodyItem::Unknown { expr, .. } => block_form(model, index, expr).map(Declared::Block),
        _ => None,
    }
}

/// `after_commit(on: :create) { … }` / `after_commit -> { … }, on: …`.
fn block_form(model: &Model, index: usize, expr: &Expr) -> Option<BlockForm> {
    let ExprNode::Send { recv: None, method, args, block, .. } = &*expr.node else { return None };
    let (chain, enforced) = hook(method.as_str())?;
    let (callback, opts, is_block): (&Expr, &[Expr], bool) = match (block.as_ref(), &args[..]) {
        (Some(b), rest) => (b, rest, true),
        (None, [first, rest @ ..]) if matches!(&*first.node, ExprNode::Lambda { .. }) => (first, rest, false),
        _ => return None,
    };
    let opts = options(opts)?;
    let ExprNode::Lambda { params, rest_param: None, block_param: None, body, .. } = &*callback.node else {
        return None;
    };
    let mut body = body.clone();
    match &params[..] {
        [] => {}
        [record] => rebind_to_self(&mut body, record),
        _ => return None,
    }
    // The block's own `next` ends the callback, as `return` ends the
    // method it becomes (the chain ignores the value). Its `break` (and
    // a proc's `return`) would mean something else there.
    if escapes(&body, is_block) {
        return None;
    }
    next_to_return(&mut body);
    let body = super::broadcasts::rewrite_rails_broadcast_calls(body, model);
    let mut body = body;
    super::markers::rewrite_column_or_assign(&mut body, model);
    body.inherit_span(expr.span);
    let span = expr.span;
    let mut guard: Vec<Expr> = Vec::new();
    let on: &[CallbackOn] = if enforced.is_empty() { &opts.on } else { enforced };
    if let Some(on) = on_guard(on, span) {
        guard.push(on);
    }
    guard.extend(opts.conditions);
    let name = format!("_{}_transaction_callback_{index}", class_slug(model));
    let method = method_def(model, &name, MethodReceiver::Instance, body, Some(void()));
    Some(BlockForm {
        entry: Entry {
            chain,
            dedupe: None,
            guard,
            call: self_send(&name, Vec::new(), span),
            prepend: opts.prepend,
            span,
        },
        method,
    })
}

struct Options {
    on: Vec<CallbackOn>,
    conditions: Vec<Expr>,
    prepend: bool,
}

/// A block form's option hash, read as the symbol form's is at ingest.
fn options(args: &[Expr]) -> Option<Options> {
    let mut out = Options { on: Vec::new(), conditions: Vec::new(), prepend: false };
    let entries = match args {
        [] => return Some(out),
        [hash] => match &*hash.node {
            ExprNode::Hash { entries, .. } => entries,
            _ => return None,
        },
        _ => return None,
    };
    let mut unless: Vec<Expr> = Vec::new();
    for (key, value) in entries {
        let ExprNode::Lit { value: Literal::Sym { value: key } } = &*key.node else { return None };
        match key.as_str() {
            "on" => {
                let values: Vec<&Expr> = match &*value.node {
                    ExprNode::Array { elements, .. } => elements.iter().collect(),
                    _ => vec![value],
                };
                out.on.clear();
                for v in values {
                    let ExprNode::Lit { value: Literal::Sym { value: action } } = &*v.node else { return None };
                    out.on.push(match action.as_str() {
                        "create" => CallbackOn::Create,
                        "update" => CallbackOn::Update,
                        "destroy" => CallbackOn::Destroy,
                        _ => return None,
                    });
                }
            }
            "if" | "unless" => {
                let values: Vec<&Expr> = match &*value.node {
                    ExprNode::Array { elements, .. } => elements.iter().collect(),
                    _ => vec![value],
                };
                for v in values {
                    let cond = condition(v)?;
                    if key.as_str() == "if" {
                        out.conditions.push(cond);
                    } else {
                        unless.push(not(cond));
                    }
                }
            }
            "prepend" => {
                out.prepend = match &*value.node {
                    ExprNode::Lit { value: Literal::Bool { value } } => *value,
                    _ => return None,
                };
            }
            _ => {}
        }
    }
    out.conditions.extend(unless);
    Some(out)
}

/// One `if:`/`unless:` value: a symbol (the predicate method), or a
/// lambda of no parameter or one (the record) whose body is one
/// expression.
fn condition(value: &Expr) -> Option<Expr> {
    match &*value.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(self_send(value.as_str(), Vec::new(), Span::synthetic())),
        ExprNode::Lambda { params, rest_param: None, block_param: None, body, .. } => {
            if matches!(&*body.node, ExprNode::Seq { .. }) || escapes(body, true) {
                return None;
            }
            let mut body = body.clone();
            match &params[..] {
                [] => {}
                [record] => rebind_to_self(&mut body, record),
                _ => return None,
            }
            Some(body)
        }
        _ => None,
    }
}

/// `transaction_include_any_action?(on)`.
fn on_guard(on: &[CallbackOn], span: Span) -> Option<Expr> {
    on.iter()
        .map(|action| {
            let predicate = match action {
                CallbackOn::Create => "_transaction_include_create?",
                CallbackOn::Update => "_transaction_include_update?",
                CallbackOn::Destroy => "_transaction_include_destroy?",
            };
            // Explicit `self.`: the predicates live on Base, so the
            // strict targets resolve them as inherited methods, not
            // as locals.
            send(Some(Expr::new(span, ExprNode::SelfRef)), predicate, Vec::new(), span)
        })
        .reduce(|left, right| bool_op(BoolOpKind::Or, left, right))
}

/// The call of a callback object. A constant is sent the chain's method
/// directly; any other expression is built once, by a memoizing class
/// method of the declaring model.
fn object_call(model: &Model, index: usize, object: &Expr, chain: Chain, span: Span) -> (Expr, Option<MethodDef>) {
    let self_arg = Expr::new(span, ExprNode::SelfRef);
    if matches!(&*object.node, ExprNode::Const { .. }) {
        return (send(Some(object.clone()), chain.object_method(), vec![self_arg], span), None);
    }
    let name = format!("_{}_transaction_callback_object_{index}", class_slug(model));
    let memo = Expr::new(
        span,
        ExprNode::OpAssign {
            target: LValue::Ivar { name: Symbol::from(format!("@{name}").as_str()) },
            op: OpAssignOp::OrOr,
            value: object.clone(),
        },
    );
    let owner = Expr::new(
        span,
        ExprNode::Const { path: model.name.0.as_str().split("::").map(Symbol::from).collect() },
    );
    let receiver = send(Some(owner), &name, Vec::new(), span);
    (
        send(Some(receiver), chain.object_method(), vec![self_arg], span),
        Some(method_def(model, &name, MethodReceiver::Class, memo, None)),
    )
}

fn statement(entry: Entry) -> Expr {
    let Entry { guard, call, span, .. } = entry;
    match guard.into_iter().reduce(|left, right| bool_op(BoolOpKind::And, left, right)) {
        None => call,
        Some(cond) => Expr::new(
            span,
            ExprNode::If {
                cond,
                then_branch: call,
                else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            },
        ),
    }
}

/// Does `body` leave its block other than by finishing: `next`,
/// `break`, `redo`, `retry` (and `return`, for a proc block, where
/// Rails raises LocalJumpError)? A nested block or lambda has its own.
fn escapes(body: &Expr, proc_return: bool) -> bool {
    let mut found = false;
    fn walk(e: &Expr, proc_return: bool, in_loop: bool, found: &mut bool) {
        match &*e.node {
            // The body's own `next` becomes `return` (`next_to_return`).
            ExprNode::Next { .. } if !in_loop => {
                if let ExprNode::Next { value: Some(v) } = &*e.node {
                    walk(v, proc_return, in_loop, found);
                }
            }
            ExprNode::Next { .. } | ExprNode::Break { .. } | ExprNode::Redo | ExprNode::Retry => *found = true,
            ExprNode::Return { .. } if proc_return => *found = true,
            ExprNode::Lambda { .. } => {}
            ExprNode::While { .. } => e.node.for_each_child(&mut |c| walk(c, proc_return, true, found)),
            _ => e.node.for_each_child(&mut |c| walk(c, proc_return, in_loop, found)),
        }
    }
    walk(body, proc_return, false, &mut found);
    found
}

/// The callback body's own `next` (not one inside a nested block or a
/// loop, which ends that) becomes `return` with the same value.
fn next_to_return(expr: &mut Expr) {
    match &mut *expr.node {
        ExprNode::Next { value } => {
            let value = value
                .take()
                .unwrap_or_else(|| Expr::new(expr.span, ExprNode::Lit { value: Literal::Nil }));
            let span = expr.span;
            *expr = Expr::new(span, ExprNode::Return { value });
        }
        ExprNode::Lambda { .. } | ExprNode::While { .. } => {}
        _ => expr.node.for_each_child_mut(&mut next_to_return),
    }
}

/// The record parameter's reads become `self` (`instance_exec(record)`).
fn rebind_to_self(expr: &mut Expr, name: &Symbol) {
    if matches!(&*expr.node, ExprNode::Var { name: n, .. } if n == name) {
        *expr = Expr::new(expr.span, ExprNode::SelfRef);
        return;
    }
    expr.node.for_each_child_mut(&mut |child| rebind_to_self(child, name));
}

fn class_slug(model: &Model) -> String {
    crate::naming::snake_case(model.name.0.as_str())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn self_send(method: &str, args: Vec<Expr>, span: Span) -> Expr {
    send(None, method, args, span)
}

fn send(recv: Option<Expr>, method: &str, args: Vec<Expr>, span: Span) -> Expr {
    Expr::new(
        span,
        ExprNode::Send { recv, method: Symbol::from(method), args, block: None, parenthesized: false },
    )
}

fn not(cond: Expr) -> Expr {
    send(Some(cond), "!", Vec::new(), Span::synthetic())
}

fn bool_op(op: BoolOpKind, left: Expr, right: Expr) -> Expr {
    Expr::new(Span::synthetic(), ExprNode::BoolOp { op, surface: BoolOpSurface::Symbol, left, right })
}

/// The chains and the block methods return nothing, as the runtime's
/// hooks they stand beside.
fn void() -> crate::ty::Ty {
    super::fn_sig(Vec::new(), crate::ty::Ty::Nil)
}

fn method_def(model: &Model, name: &str, receiver: MethodReceiver, body: Expr, signature: Option<crate::ty::Ty>) -> MethodDef {
    MethodDef {
        visibility: MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver,
        params: Vec::new(),
        body,
        signature,
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}
