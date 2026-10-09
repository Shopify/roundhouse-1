//! Lower `app.seeds` (the typed `db/seeds.rb` Expr) into a `Seeds`
//! LibraryClass with a single class method `run()`. The body is the
//! seeds Expr verbatim — analyze has already attached types and
//! effects, so the walker emits `Article.create!(...)`, etc., the
//! same way it would in a controller body.
//!
//! Self-describing IR: the seeds body's typing was set during
//! analyze (DbWrite on `create!`, etc.). The lowerer just wraps it
//! in a MethodDef shell — no per-target string-shape decisions.
//!
//! ONE IR rewrite happens here: Rails' has-many `create` shorthand
//! (`article.comments.create!(...)`) gets de-magic'd into the
//! explicit `Comment.create!(article_id: article.id, ...)` form
//! that targets without CollectionProxy semantics can dispatch.
//! The Ruby source stays Rails-idiomatic; the lowerer translates.
//! Mirrors `rewrite_assoc_through_parent_typed` in the controller
//! lowerer (which handles `build`/`find` for assignment-shape
//! contexts) — seeds are statement-shape so this pass only handles
//! the `create` form on its own.

use crate::App;
use crate::dialect::LibraryFunction;
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode};
use crate::ident::Symbol;
use crate::lower::typing::fn_sig;
use crate::span::Span;
use crate::ty::Ty;

/// Build the `Seeds` module as a single LibraryFunction:
/// `Seeds.run() -> nil` carrying the typed seeds Expr from
/// `app.seeds` verbatim. Empty when the app has no seeds file.
pub fn lower_seeds_to_library_functions(app: &App) -> Vec<LibraryFunction> {
    let Some(body) = app.seeds.as_ref().cloned() else {
        return Vec::new();
    };
    let body = rewrite_assoc_create(&body);
    let module_path = vec![Symbol::from("Seeds")];
    vec![LibraryFunction {
        module_path,
        name: Symbol::from("run"),
        params: Vec::new(),
        unsupported_formals: None,
        has_anonymous_block: false,
        body,
        signature: Some(fn_sig(vec![], Ty::Nil)),
        effects: EffectSet::default(),
        is_async: false,
    }]
}

/// `<parent>.<assoc>.create(<args>)` → `<AssocClass>.create(<parent_class>_id: <parent>.id, ...args)`.
///
/// Pattern shape (after typer):
///   Send {                                     ← outer (create/create!)
///     recv: Some(Send {                        ← inner (assoc reader)
///       recv: Some(parent_expr),               ← typed Ty::Class { id: <Parent> }
///       method: <assoc> (e.g. "comments"),
///       args: [],
///     }),
///     method: "create" | "create!",
///     args: [Hash{...}] | [],
///   }
///
/// Transforms to:
///   Send {
///     recv: Some(Const(<AssocClass>)),
///     method: "create" | "create!",
///     args: [Hash{ <parent_class>_id: parent_expr.id, ...original_entries }],
///   }
///
/// `<AssocClass>` is the association's DECLARED target when the parent
/// model is known — `has_many :push_subscriptions, class_name:
/// "Push::Subscription"` names a class the assoc name alone never
/// reaches (campfire's test built a `PushSubscription`, an
/// uninitialized constant) — and singularize+camelize on the assoc
/// method name otherwise. `<parent_class>_id` derives from the
/// parent_expr's type (a Class type the typer set), snake_cased — e.g.
/// `Article` → `article_id`. Falls through (returns the expr
/// unchanged) when any of the shape preconditions don't hold.
pub fn rewrite_assoc_create(expr: &Expr) -> Expr {
    rewrite_assoc_create_with_models(expr, &[])
}

pub fn rewrite_assoc_create_with_models(expr: &Expr, models: &[crate::dialect::Model]) -> Expr {
    crate::lower::controller_to_library::util::map_expr(expr, &|e| assoc_create_rewrite(e, models))
}

/// In-place twin. Returns whether any node changed so the test
/// lowerer can skip a follow-up typing pass.
///
/// `join_target` is for a tree whose has_many reader keeps what it
/// loads (`emit::ruby::library::apply_collection_memoization`): the
/// constructed record then joins the owner's target, as Rails'
/// `add_to_target` adds it, through the runtime's
/// `ActiveRecord::Associations::Association.created` — what the
/// scope-chain lowering writes for the same call in an app body. Here
/// it rides a `tap` on the constructor, so the expression keeps the
/// constructor's type: a test body goes on to hand the record to type-
/// directed rewrites (`comment_path(comment)` projecting `comment.id`)
/// after this pass, which an untyped runtime call would decline.
/// Without it a test that read the association, created through it and
/// read it again saw the first read's rows.
pub fn rewrite_assoc_create_with_models_in_place(
    expr: &mut Expr,
    models: &[crate::dialect::Model],
    join_target: bool,
) -> bool {
    crate::lower::controller_to_library::util::map_expr_mut(expr, &|e| {
        let created = assoc_create_rewrite(e, models)?;
        Some(if join_target { join_owner_target(e, created).unwrap_or_else(|c| c) } else { created })
    })
}

/// `<ctor>` → `<ctor>.tap { |__rh_record| ActiveRecord::Associations::
/// Association.created(<owner>, :<assoc>, __rh_record) }`, for the
/// original `<owner>.<assoc>.<create|create!|build>(…)` site `orig`.
/// Only for an owner the code can name again — a local, an ivar,
/// `self`, or one zero-argument read off one of those or a constant
/// (`ArticlesFixtures.one`, which memoizes) — as the scope-chain
/// lowering's `renamable` test: any other owner expression is a fresh
/// record nothing reads afterwards. Hands `ctor` back otherwise.
fn join_owner_target(orig: &Expr, ctor: Expr) -> Result<Expr, Expr> {
    let ExprNode::Send { recv: Some(outer_recv), .. } = &*orig.node else { return Err(ctor) };
    let ExprNode::Send { recv: Some(owner), method: assoc, .. } = &*outer_recv.node else {
        return Err(ctor);
    };
    let renamable = match &*owner.node {
        ExprNode::Var { .. } | ExprNode::Ivar { .. } | ExprNode::SelfRef => true,
        ExprNode::Send { recv, args, block: None, .. } if args.is_empty() => match recv {
            None => true,
            Some(root) => matches!(
                &*root.node,
                ExprNode::Const { .. } | ExprNode::SelfRef | ExprNode::Var { .. } | ExprNode::Ivar { .. }
            ),
        },
        _ => false,
    };
    if !renamable {
        return Err(ctor);
    }
    let span = orig.span;
    let param = Symbol::from("__rh_record");
    let call = Expr::new(
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
            method: Symbol::from("created"),
            args: vec![
                owner.clone(),
                Expr::new(span, ExprNode::Lit { value: crate::expr::Literal::Sym { value: assoc.clone() } }),
                Expr::new(span, ExprNode::Var { id: crate::ident::VarId(0), name: param.clone() }),
            ],
            block: None,
            parenthesized: true,
        },
    );
    let block = Expr::new(
        span,
        ExprNode::Lambda {
            signature: Default::default(),
            extra_params: Vec::new(),
            rest_param: None,
            params: vec![param],
            block_param: None,
            body: call,
            // Braces bind to `tap` wherever the call stands, also as
            // an unparenthesized argument (`do … end` would bind to the
            // outer call).
            block_style: crate::expr::BlockStyle::Brace,
        },
    );
    Ok(Expr::new(
        span,
        ExprNode::Send {
            recv: Some(ctor),
            method: Symbol::from("tap"),
            args: vec![],
            block: Some(block),
            parenthesized: false,
        },
    ))
}

fn assoc_create_rewrite(e: &Expr, models: &[crate::dialect::Model]) -> Option<Expr> {
    let declared_target = |parent: &str, assoc: &str| -> Option<String> {
        let model = models.iter().find(|m| m.name.0.as_str() == parent)?;
        model.associations().find_map(|a| match a {
            crate::dialect::Association::HasMany { name, target, .. }
                if name.as_str() == assoc =>
            {
                Some(target.0.as_str().to_string())
            }
            _ => None,
        })
    };
    let ExprNode::Send {
        recv: Some(outer_recv),
        method: outer_method,
        args: outer_args,
        block: None,
        ..
    } = &*e.node
    else {
        return None;
    };
    let outer_method_str = outer_method.as_str();
    // `create` / `create!` map to the same-named class methods.
    // `build` is Rails-specific (instantiate without saving) —
    // map to `new` so Ruby/CRuby callers get `<Class>.new(attrs)`
    // and the TS emitter renders that as `new <Class>(attrs)`.
    let target_method = match outer_method_str {
        "create" | "create!" => outer_method.clone(),
        "build" => Symbol::from("new"),
        _ => return None,
    };
    let ExprNode::Send {
        recv: Some(parent_expr),
        method: assoc_method,
        args: inner_args,
        block: None,
        ..
    } = &*outer_recv.node
    else {
        return None;
    };
    if !inner_args.is_empty() {
        return None;
    }
    // Resolve parent class from the typer-set type. Without the
    // class type we can't derive the FK name; fall through.
    let parent_class = match parent_expr.ty.as_ref() {
        Some(Ty::Class { id, .. }) => id.0.as_str().to_string(),
        _ => return None,
    };
    let assoc_class = declared_target(&parent_class, assoc_method.as_str())
        .unwrap_or_else(|| crate::naming::singularize_camelize(assoc_method.as_str()));
    let fk = format!("{}_id", crate::naming::snake_case(&parent_class));

    // Build the FK-id expr: `<parent_expr>.id`.
    let parent_id = Expr::new(
        parent_expr.span,
        ExprNode::Send {
            recv: Some(parent_expr.clone()),
            method: Symbol::from("id"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    let fk_entry = (
        Expr::new(
            e.span,
            ExprNode::Lit { value: crate::expr::Literal::Sym { value: Symbol::from(fk) } },
        ),
        parent_id,
    );

    // Merge FK entry with original kwargs/hash. Real-blog's
    // `article.comments.create!(commenter:, body:)` parses the
    // trailing kwargs as a single Hash arg.
    let merged_hash = match outer_args.first().map(|a| (&*a.node, a.span)) {
        Some((ExprNode::Hash { entries, kwargs }, span)) => {
            let mut new_entries = vec![fk_entry];
            new_entries.extend(entries.iter().cloned());
            Expr::new(span, ExprNode::Hash { entries: new_entries, kwargs: *kwargs })
        }
        _ => Expr::new(
            e.span,
            ExprNode::Hash { entries: vec![fk_entry], kwargs: true },
        ),
    };

    Some(Expr::new(
        e.span,
        ExprNode::Send {
            recv: Some(Expr::new(
                e.span,
                ExprNode::Const {
                    path: assoc_class.split("::").map(Symbol::from).collect(),
                },
            )),
            method: target_method,
            args: vec![merged_hash],
            block: None,
            parenthesized: true,
        },
    ))
}

// `Span` used in synthesized expressions above.
#[allow(unused_imports)]
use Span as _Span;
