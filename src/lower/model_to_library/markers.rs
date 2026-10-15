//! Unknown body items recognized as Rails markers. Most Unknowns stay
//! dropped (they're emitter responsibility or future-lowerer work), but
//! a small set carry semantics that translate cleanly into method
//! definitions on the lowered class.
//!
//! Lifecycle callbacks, both forms: symbol-form declarations
//! (`before_save :check_session_token` — ingested as
//! `ModelBodyItem::Callback`) lower to self-calls inside a `def
//! hook_name` override of the runtime Base's no-op hook, and
//! block-form ones (`after_create_commit { … }` — Unknown body items,
//! parse_callback rejects them) lower to `def hook_name; <block-
//! body>; end`. Multiple sources can target the same hook (either
//! callback form + broadcasts_to expansion + dependent: :destroy
//! cascade); when this lowering finds an existing method with the
//! matching name it folds the new body into that method's Seq,
//! preserving source order across sources.

use crate::dialect::{AccessorKind, MethodDef, MethodReceiver, Model, ModelBodyItem, Param, Touch};
use crate::schema::Schema;
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{Symbol, VarId};
use crate::span::Span;
use crate::ty::Ty;

use super::{fn_sig, seq, with_ty};

/// Per-model `dom_prefix` instance method returning the snake_case
/// model name as a String literal. Used by
/// `ActionView::ViewHelpers.dom_id(record)` to build CSS-id strings
/// at transpile time rather than via runtime introspection
/// (`record.class.name.downcase` previously). Skipped for abstract
/// models (`primary_abstract_class` marker present) — those are never
/// instantiated, and ApplicationRecord's lowered shape is tested
/// against the abstract-marker-only baseline.
pub(super) fn push_dom_prefix_method(methods: &mut Vec<MethodDef>, model: &Model, models: &[Model]) {
    if is_primary_abstract_class(model) {
        return;
    }
    // Rails' `param_key` for a namespaced model joins the segments with
    // `_` (`Admin::Widget` -> `admin_widget`), never `admin::widget`.
    let prefix = model
        .name
        .0
        .as_str()
        .split("::")
        .map(crate::naming::snake_case)
        .collect::<Vec<_>>()
        .join("_");
    // An STI base's rows belong to subclasses, and Rails' dom_class
    // answers the SUBCLASS (`Rooms::Open` rows are `rooms_open`, on
    // the page and in every broadcast target). Hydration here is
    // base-classed, so the base's prefix dispatches on the type
    // column — the same stamp the subclass constructors write and
    // `sti_scope`'s relation rewrites filter by. A row whose type
    // names no known subclass (or a plain base row) keeps the base's
    // own prefix, which is also Rails' answer for a base-classed row.
    let body = if model.sti_subclass_names.is_empty() {
        Expr::new(
            Span::synthetic(),
            ExprNode::Lit { value: Literal::Str { value: prefix } },
        )
    } else {
        let mut arms: Vec<crate::expr::Arm> = model
            .sti_subclass_names
            .iter()
            .map(|sub| {
                let fqn = sub.0.as_str();
                let dom_class = fqn
                    .split("::")
                    .map(crate::naming::snake_case)
                    .collect::<Vec<_>>()
                    .join("_");
                crate::expr::Arm {
                    pattern: crate::expr::Pattern::Lit {
                        value: Literal::Str { value: fqn.to_string() },
                    },
                    guard: None,
                    body: with_ty(
                        Expr::new(
                            Span::synthetic(),
                            ExprNode::Lit { value: Literal::Str { value: dom_class } },
                        ),
                        Ty::Str,
                    ),
                }
            })
            .collect();
        arms.push(crate::expr::Arm {
            pattern: crate::expr::Pattern::Wildcard,
            guard: None,
            body: with_ty(
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Lit { value: Literal::Str { value: prefix } },
                ),
                Ty::Str,
            ),
        });
        Expr::new(
            Span::synthetic(),
            ExprNode::Case {
                scrutinee: Expr::new(
                    Span::synthetic(),
                    ExprNode::Ivar { name: Symbol::from(crate::lower::sti_scope::inheritance_column(model, models)) },
                ),
                arms,
            },
        )
    };
    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("dom_prefix"),
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body: with_ty(body, Ty::Str),
        signature: Some(fn_sig(vec![], Ty::Str)),
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
            mutates_self: false,
            block_param: None,
    });
}

/// Per-model `to_param` — Rails gives every `ActiveRecord::Base` one
/// (`id&.to_s`), and every URL helper stringifies a record through it.
/// Runs AFTER `push_user_methods` with a skip on an existing instance
/// method of the name, so a model's own override wins (lobsters'
/// `User#to_param` answers the username). Skipped for abstract models
/// like the other instance-shaped synthesizers.
///
/// `@id.to_s`, not `id&.to_s`: the ivar is the lowered id slot every
/// model carries, and on a persisted record — the only kind a URL
/// helper ever renders — the two are byte-identical. (For an unsaved
/// record Rails answers nil where this answers ""; no corpus call
/// site reaches that.) Belongs to every target: the definition that
/// existed before lived in the CRuby overlay's core_ext, which the
/// strict targets never apply — so campfire's avatar helper
/// (`Zlib.crc32(user.to_param)`) 500'd every avatar on the binary.
pub(super) fn push_to_param_method(methods: &mut Vec<MethodDef>, model: &Model) {
    if is_primary_abstract_class(model) {
        return;
    }
    if methods
        .iter()
        .any(|m| m.name.as_str() == "to_param" && m.receiver == MethodReceiver::Instance)
    {
        return;
    }
    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("to_param"),
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body: with_ty(
            Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: Some(Expr::new(
                        Span::synthetic(),
                        ExprNode::Ivar { name: Symbol::from("id") },
                    )),
                    method: Symbol::from("to_s"),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            ),
            Ty::Str,
        ),
        signature: Some(fn_sig(vec![], Ty::Str)),
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    });
}

/// Per-model `dom_record_key` — the identity half of `dom_id`, as one
/// String. Rails' `dom_id` derives it from `record.to_key.join("_")`,
/// which a model may override: campfire's `Message#to_key` answers
/// `[client_message_id]`, and that is the mechanism by which a
/// broadcast row carries the SAME dom id the sender's optimistic
/// client-side echo minted — Turbo's append then replaces the echo
/// instead of standing a duplicate beside it. The runtime's `dom_id`
/// used `record.id` directly, so every message id on the emitted lanes
/// diverged from Rails and the sender's tab could show its message
/// twice (found by `scripts/campfire-compare`).
///
/// Synthesized rather than read dynamically because the strict targets
/// want one String per model, not `Array[Integer] | Array[String?]`
/// unioning across a poly slot: a model with its own `to_key` gets
/// `to_key.join("_")` (typed by ITS return), everything else gets
/// `@id.to_s`. Runs after `push_user_methods` so the check can see the
/// model's own `to_key` in the accumulated list.
pub(super) fn push_dom_record_key_method(methods: &mut Vec<MethodDef>, model: &Model) {
    if is_primary_abstract_class(model) {
        return;
    }
    let has_to_key = methods
        .iter()
        .any(|m| m.name.as_str() == "to_key" && m.receiver == MethodReceiver::Instance);
    let body = if has_to_key {
        // `to_key.join("_")` — Rails' record_key_for_dom_id, on the
        // model's own answer.
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("to_key"),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            )),
            method: Symbol::from("join"),
            args: vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit { value: Literal::Str { value: "_".to_string() } },
            )],
            block: None,
            parenthesized: false,
        }
    } else {
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Ivar { name: Symbol::from("id") },
            )),
            method: Symbol::from("to_s"),
            args: Vec::new(),
            block: None,
            parenthesized: false,
        }
    };
    methods.push(MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from("dom_record_key"),
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body: with_ty(Expr::new(Span::synthetic(), body), Ty::Str),
        signature: Some(fn_sig(vec![], Ty::Str)),
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    });
}

/// Primary abstract bases omit instance-shaped synthesis, unless a later
/// literal marker makes them concrete. Intermediate abstract bases still
/// emit methods for their concrete children to inherit.
fn is_primary_abstract_class(model: &Model) -> bool {
    model.body.iter().any(|item| {
        if let ModelBodyItem::Unknown { expr, .. } = item {
            if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
                return args.is_empty() && method.as_str() == "primary_abstract_class";
            }
        }
        false
    }) && is_abstract_class(model)
}

/// Literal abstract markers take effect in declaration order. Admission
/// requires a concrete includer; production separately checks whether
/// the model is a primary base before suppressing inherited methods.
pub(crate) fn is_abstract_class(model: &Model) -> bool {
    abstract_class_setting(model) == Some(true)
}

/// The value the class body last gave `abstract_class`, from literal
/// markers only: `self.abstract_class = true|false`, or
/// `primary_abstract_class` (which sets it true). `None` when the body
/// never sets it, so `abstract_class` reads nil as in Rails.
pub(crate) fn abstract_class_setting(model: &Model) -> Option<bool> {
    let mut abstract_class = None;
    for item in &model.body {
        if let ModelBodyItem::Unknown { expr, .. } = item {
            if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
                if args.is_empty() && method.as_str() == "primary_abstract_class" {
                    abstract_class = Some(true);
                }
            } else if let Some(value) = literal_abstract_class_write(expr) {
                abstract_class = Some(value);
            }
        }
    }
    abstract_class
}

/// `self.abstract_class = <true|false>`: the literal write the class
/// body replays (see `emit::ruby::attribute_api`).
pub(crate) fn literal_abstract_class_write(expr: &Expr) -> Option<bool> {
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*expr.node else {
        return None;
    };
    if !matches!(&*recv.node, ExprNode::SelfRef) || method.as_str() != "abstract_class=" {
        return None;
    }
    match args.as_slice() {
        [arg] => match &*arg.node {
            ExprNode::Lit { value: Literal::Bool { value } } => Some(*value),
            _ => None,
        },
        _ => None,
    }
}

/// `attr_accessor :vote` / `attr_reader :x` / `attr_writer :y` on a model
/// — virtual (non-column) attributes Rails backs with plain ivars. Lower
/// each to a getter `def name; @name; end` and/or setter `def name=(value);
/// @name = value; end`. No schema/RBS anchors the type, so they stay
/// Untyped (fine for the dynamic targets; strict targets gain a typed
/// virtual-attribute story when an app that uses them is brought up there).
/// Skips any name a column/def/association already defined, and skips
/// abstract base classes.
/// The symbols in `X = %i[a b c]` declared in this model's own body.
/// Empty for anything that is not a literal array of symbols — a
/// computed list is not something to guess at.
fn const_symbol_array(model: &Model, path: &[Symbol]) -> Vec<Symbol> {
    let Some(wanted) = path.last() else { return Vec::new() };
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Assign { target: LValue::Const { path: target }, value } = &*expr.node else {
            continue;
        };
        if target.last() != Some(wanted) {
            continue;
        }
        let ExprNode::Array { elements, .. } = &*value.node else { continue };
        return elements
            .iter()
            .filter_map(|e| match &*e.node {
                ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
                _ => None,
            })
            .collect();
    }
    Vec::new()
}

/// The names one `attr_accessor` / `attr_reader` / `attr_writer` call
/// declares, splat included.
///
/// `attr_accessor *ATTRIBUTES` — the splat form, which campfire's
/// `Opengraph::Metadata` uses to name its four fields from the constant
/// right above it. Expanded from that constant's own literal in this
/// class body: a model has no `constants` map (an `X = [...]` lands in
/// `ModelBodyItem::Unknown`), so the fold reads the assignment where it
/// sits. Unexpanded, the class emitted NO accessors at all and every
/// read of `title` was a NoMethodError.
fn accessor_names(model: &Model, args: &[Expr]) -> Vec<Symbol> {
    let mut names: Vec<Symbol> = Vec::new();
    for arg in args {
        match &*arg.node {
            ExprNode::Lit { value: Literal::Sym { value } } => names.push(value.clone()),
            ExprNode::Splat { value } => {
                if let ExprNode::Const { path } = &*value.node {
                    names.extend(const_symbol_array(model, path));
                }
            }
            _ => {}
        }
    }
    names
}

/// Every attribute this model declares through the `attr_*` family, in
/// declaration order. The constructor `ActiveModel::Model` supplies
/// assigns exactly these.
///
/// `pub(crate)` because `lower::as_json_poro` needs the same list and
/// must not re-derive it: the splat expansion below is the only place
/// that knows `attr_accessor *ATTRIBUTES` names four fields, and a
/// second copy of that would go stale the first time either moved.
pub(crate) fn declared_attr_names(model: &Model) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
            continue;
        };
        if !matches!(method.as_str(), "attr_accessor" | "attr_reader" | "attr_writer") {
            continue;
        }
        for name in accessor_names(model, args) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

pub(super) fn push_attr_accessor_methods(methods: &mut Vec<MethodDef>, model: &Model) {
    if is_primary_abstract_class(model) {
        return;
    }
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
            continue;
        };
        let (want_reader, want_writer) = match method.as_str() {
            "attr_accessor" => (true, true),
            "attr_reader" => (true, false),
            "attr_writer" => (false, true),
            _ => continue,
        };
        // `attr_accessor *ATTRIBUTES` — the splat form, which campfire's
        // `Opengraph::Metadata` uses to name its four fields from the
        // constant right above it. Expanded from that constant's own
        // literal in this class body: a model has no `constants` map
        // (an `X = [...]` lands in `ModelBodyItem::Unknown`), so the
        // fold reads the assignment where it sits. Unexpanded, the
        // class emitted NO accessors at all and every read of `title`
        // was a NoMethodError.
        //
        // A `def` of the same name REPLACES the accessor — Ruby's last
        // definition wins, and `attr_accessor :foo` is a definition.
        // Checking only `methods` is not enough: without yielding here,
        // an accessor pushed for a name the model defines is later
        // eligible for replacement by `push_user_methods` only when it
        // still has the bare-ivar attr_* shape; skipping the push when
        // `model_defines_instance_method` is the primary gate so the
        // app's memo body is what `methods` carries.
        //
        // Measured on campfire's `Opengraph::Location`, which declares
        // `attr_accessor :url, :parsed_url` and then memoizes
        // `def parsed_url; @parsed_url ||= URI.parse(url); end`. With the
        // accessor winning, `parsed_url` was permanently nil, so
        // `validate_url` failed for every URL and `valid?` was false
        // throughout the subsystem — 9 of its own tests, none of which
        // named a missing method: they read as bare assertion failures.
        //
        // `model_defines_instance_method` is the same yield the temporal
        // writer in `schema.rs` already performs, and what
        // `push_synth_instance_method` documents as the rule for every
        // late synthesizer. This one never got it.
        let names = accessor_names(model, args);
        for name in &names {
            let setter = Symbol::from(format!("{}=", name.as_str()));
            let defines = |n: &Symbol| {
                crate::lower::model_to_library::model_defines_instance_method(model, n)
            };
            if want_reader && !defines(name) && !methods.iter().any(|m| m.name == *name) {
                methods.push(MethodDef {
                    visibility: crate::dialect::MethodVisibility::Public,
                    unsupported_formals: None,
                    has_anonymous_block: false,
                    name_span: crate::span::Span::synthetic(),
                    name: name.clone(),
                    receiver: MethodReceiver::Instance,
                    params: Vec::new(),
                    body: Expr::new(expr.span, ExprNode::Ivar { name: name.clone() }),
                    signature: None,
                    effects: EffectSet::default(),
                    enclosing_class: Some(model.name.0.clone()),
                    kind: AccessorKind::AttributeReader,
                    is_async: false,
                    mutates_self: false,
                    block_param: None,
                });
            }
            if want_writer && !defines(&setter) && !methods.iter().any(|m| m.name == setter) {
                let value = Symbol::from("value");
                methods.push(MethodDef {
                    visibility: crate::dialect::MethodVisibility::Public,
                    unsupported_formals: None,
                    has_anonymous_block: false,
                    name_span: crate::span::Span::synthetic(),
                    name: setter,
                    receiver: MethodReceiver::Instance,
                    params: vec![Param::positional(value.clone())],
                    body: Expr::new(
                        expr.span,
                        ExprNode::Assign {
                            target: LValue::Ivar { name: name.clone() },
                            value: Expr::new(expr.span, ExprNode::Var { id: VarId(0), name: value }),
                        },
                    ),
                    signature: None,
                    effects: EffectSet::default(),
                    enclosing_class: Some(model.name.0.clone()),
                    kind: AccessorKind::AttributeWriter,
                    is_async: false,
                    mutates_self: true,
                    block_param: None,
                });
            }
        }
    }
}

/// `attribute :name, :type` declarations (the Rails Attributes API) in
/// a model body — `(name, type)` pairs, both symbol literals. The
/// 2-arg form only; `default:`-carrying declarations stay unclaimed
/// (and warned) until a fixture demands them. Shared with the view
/// lowerer's `bool_reader_names` (a `:boolean` attribute is a bool
/// reader for `f.check_box`) and the permit-writer filter (an
/// `attribute` writer is assignable).
pub(crate) fn attribute_api_decls(body: &[ModelBodyItem]) -> Vec<(Symbol, Symbol)> {
    let mut out = Vec::new();
    for item in body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
            continue;
        };
        if method.as_str() != "attribute" || args.len() != 2 {
            continue;
        }
        let (
            ExprNode::Lit { value: Literal::Sym { value: name } },
            ExprNode::Lit { value: Literal::Sym { value: ty } },
        ) = (&*args[0].node, &*args[1].node)
        else {
            continue;
        };
        out.push((name.clone(), ty.clone()));
    }
    out
}

/// `attribute :name, :type` — typed virtual attributes (lobsters'
/// `attribute :mod_note, :boolean` on Message, `:is_unread` on the
/// SQL-view-backed ReplyingComment). Reader is a typed ivar read;
/// the `:boolean` writer applies Rails' Type::Boolean cast over the
/// realistic value space via to_s (`"" / "0" / "false" / "f"` →
/// false, anything else → true — the form roundtrip assigns "0"/"1"
/// strings, and an uncast write would leave "0" truthy). Other types
/// assign verbatim. A custom method in the model body wins (the
/// synthesizers run before `push_user_methods`, which drops
/// collisions — same dance as attr_accessor).
pub(super) fn push_attribute_api_methods(methods: &mut Vec<MethodDef>, model: &Model) {
    if is_primary_abstract_class(model) {
        return;
    }
    for (name, ty_sym) in attribute_api_decls(&model.body) {
        let is_bool = ty_sym.as_str() == "boolean";
        let setter = Symbol::from(format!("{}=", name.as_str()));
        if !methods.iter().any(|m| m.name == name) {
            methods.push(MethodDef {
                visibility: crate::dialect::MethodVisibility::Public,
                unsupported_formals: None,
                has_anonymous_block: false,
                name_span: crate::span::Span::synthetic(),
                name: name.clone(),
                receiver: MethodReceiver::Instance,
                params: Vec::new(),
                body: if is_bool {
                    with_ty(
                        Expr::new(Span::synthetic(), ExprNode::Ivar { name: name.clone() }),
                        Ty::Bool,
                    )
                } else {
                    Expr::new(Span::synthetic(), ExprNode::Ivar { name: name.clone() })
                },
                signature: if is_bool {
                    Some(super::fn_sig(vec![], Ty::Bool))
                } else {
                    None
                },
                effects: EffectSet::default(),
                enclosing_class: Some(model.name.0.clone()),
                kind: AccessorKind::AttributeReader,
                is_async: false,
                mutates_self: false,
                block_param: None,
            });
        }
        if !methods.iter().any(|m| m.name == setter) {
            let value = Symbol::from("value");
            let value_ref = Expr::new(
                Span::synthetic(),
                ExprNode::Var { id: VarId(0), name: value.clone() },
            );
            let body = if is_bool {
                // s = value.to_s
                // @name = (s == "" || s == "0" || s == "false" || s == "f" ? false : true)
                let s = Symbol::from("s");
                let s_ref = |_: ()| {
                    Expr::new(
                        Span::synthetic(),
                        ExprNode::Var { id: VarId(0), name: s.clone() },
                    )
                };
                let to_s = Expr::new(
                    Span::synthetic(),
                    ExprNode::Send {
                        recv: Some(value_ref),
                        method: Symbol::from("to_s"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                );
                let assign_s = Expr::new(
                    Span::synthetic(),
                    ExprNode::Assign {
                        target: LValue::Var { id: VarId(0), name: s.clone() },
                        value: to_s,
                    },
                );
                let eq = |lit: &str| {
                    Expr::new(
                        Span::synthetic(),
                        ExprNode::Send {
                            recv: Some(s_ref(())),
                            method: Symbol::from("=="),
                            args: vec![Expr::new(
                                Span::synthetic(),
                                ExprNode::Lit { value: Literal::Str { value: lit.to_string() } },
                            )],
                            block: None,
                            parenthesized: false,
                        },
                    )
                };
                let or = |left: Expr, right: Expr| {
                    Expr::new(
                        Span::synthetic(),
                        ExprNode::BoolOp {
                            op: crate::expr::BoolOpKind::Or,
                            surface: crate::expr::BoolOpSurface::Symbol,
                            left,
                            right,
                        },
                    )
                };
                let falsey = or(or(or(eq(""), eq("0")), eq("false")), eq("f"));
                let cast = Expr::new(
                    Span::synthetic(),
                    ExprNode::If {
                        cond: falsey,
                        then_branch: with_ty(
                            Expr::new(
                                Span::synthetic(),
                                ExprNode::Lit { value: Literal::Bool { value: false } },
                            ),
                            Ty::Bool,
                        ),
                        else_branch: with_ty(
                            Expr::new(
                                Span::synthetic(),
                                ExprNode::Lit { value: Literal::Bool { value: true } },
                            ),
                            Ty::Bool,
                        ),
                    },
                );
                let assign = Expr::new(
                    Span::synthetic(),
                    ExprNode::Assign {
                        target: LValue::Ivar { name: name.clone() },
                        value: cast,
                    },
                );
                Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![assign_s, assign] })
            } else {
                Expr::new(
                    Span::synthetic(),
                    ExprNode::Assign {
                        target: LValue::Ivar { name: name.clone() },
                        value: value_ref,
                    },
                )
            };
            methods.push(MethodDef {
                visibility: crate::dialect::MethodVisibility::Public,
                unsupported_formals: None,
                has_anonymous_block: false,
                name_span: crate::span::Span::synthetic(),
                name: setter,
                receiver: MethodReceiver::Instance,
                params: vec![Param::positional(value)],
                body,
                signature: None,
                effects: EffectSet::default(),
                enclosing_class: Some(model.name.0.clone()),
                kind: AccessorKind::AttributeWriter,
                is_async: false,
                mutates_self: true,
                block_param: None,
            });
        }
    }
}

/// `primary_abstract_class` marks a model as the abstract base of a Rails
/// app. Lowered to `def self.abstract?; true; end` — the explicit form
/// spinel-blog's runtime expects.
pub(super) fn push_unknown_marker_methods(methods: &mut Vec<MethodDef>, model: &Model) {
    for item in &model.body {
        if let ModelBodyItem::Unknown { expr, .. } = item {
            if let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
                if args.is_empty() && method.as_str() == "primary_abstract_class" {
                    methods.push(MethodDef {
                        visibility: crate::dialect::MethodVisibility::Public,
                        unsupported_formals: None,
                        has_anonymous_block: false,
                        name_span: crate::span::Span::synthetic(),
                        name: Symbol::from("abstract?"),
                        receiver: MethodReceiver::Class,
                        params: Vec::new(),
                        body: with_ty(
                            Expr::new(
                                expr.span,
                                ExprNode::Lit { value: Literal::Bool { value: true } },
                            ),
                            Ty::Bool,
                        ),
                        signature: Some(fn_sig(vec![], Ty::Bool)),
                        effects: EffectSet::default(),
                        enclosing_class: Some(model.name.0.clone()),
                        kind: AccessorKind::AttributeReader,
                        is_async: false,
            mutates_self: false,
            block_param: None,
                    });
                }
            }
        }
    }
}

/// `self.<col> ||= value` (string column) → `if self.<col>.blank?
/// then self.<col> = value end` — see the call-site note in
/// `push_block_callback`. Recursive over the whole body; only fires
/// for `LValue::Attr` targets on `self` naming a schema string
/// column.
pub(super) fn rewrite_column_or_assign(e: &mut Expr, model: &Model) {
    use crate::expr::{LValue, OpAssignOp};
    let hit = match &*e.node {
        ExprNode::OpAssign {
            target: LValue::Attr { recv, name },
            op: OpAssignOp::OrOr,
            value,
        } if matches!(&*recv.node, ExprNode::SelfRef)
            && matches!(model.attributes.fields.get(name), Some(crate::ty::Ty::Str)) =>
        {
            Some((name.clone(), value.clone()))
        }
        _ => None,
    };
    if let Some((name, value)) = hit {
        let span = e.span;
        let self_read = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(Expr::new(span, ExprNode::SelfRef)),
                method: name.clone(),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        );
        // Grounded rather than spelled `blank?`: `lower::blank` runs
        // before this pass, so a synthesized send is never lowered and
        // never reported. The match above already established the
        // column is `Ty::Str`, which is the grounding's precondition.
        // See `blank::synthesized_string_blank`.
        let blank = crate::lower::blank::synthesized_string_blank(span, self_read);
        let assign = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(Expr::new(span, ExprNode::SelfRef)),
                method: Symbol::from(format!("{}=", name.as_str())),
                args: vec![value],
                block: None,
                parenthesized: false,
            },
        );
        *e.node = ExprNode::If {
            cond: blank,
            then_branch: assign,
            else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        };
        e.ty = None;
        return;
    }
    e.node.for_each_child_mut(&mut |c| rewrite_column_or_assign(c, model));
}

/// Look up an existing `Method` named `hook_name` and append `call` to
/// its body's Seq, OR push a new method with `call` as the body. The
/// fold preserves source order; broadcasts_to runs first so its calls
/// lead any block-form callback bodies that the next pass would add.
pub(crate) fn fold_into_or_push(methods: &mut Vec<MethodDef>, model: &Model, hook_name: &str, call: Expr) {
    let hook = Symbol::from(hook_name);
    if let Some(existing) = methods.iter_mut().find(|m| m.name == hook) {
        let mut stmts = match &*existing.body.node {
            ExprNode::Seq { exprs } => exprs.clone(),
            _ => vec![existing.body.clone()],
        };
        stmts.push(call);
        existing.body = seq(stmts);
    } else {
        methods.push(MethodDef {
            visibility: crate::dialect::MethodVisibility::Public,
            unsupported_formals: None,
            has_anonymous_block: false,
            name_span: crate::span::Span::synthetic(),
            name: hook,
            receiver: MethodReceiver::Instance,
            params: Vec::new(),
            body: call,
            signature: Some(fn_sig(vec![], Ty::Nil)),
            effects: EffectSet::default(),
            enclosing_class: Some(model.name.0.clone()),
            kind: AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }
}


/// `belongs_to :creator, class_name: "User", default: -> { Current.user }`
/// → a `before_validation` statement:
///
///   self.creator = Current.user if @creator_id.nil? || @creator_id == 0
///
/// Rails registers this as a `before_validation` at declaration time
/// (`Builder::BelongsTo.add_default_callbacks`), whose body is
/// `association(name).default(&block)` — "write the block's value when
/// the reader is nil". Without it every `Message.create!` that did not
/// name a creator failed the presence validation the SAME declaration
/// installs, which reads as the app being wrong about its own model:
/// campfire creates messages, rooms and boosts from `Current.user`
/// alone in seven tests.
///
/// The guard is on the FOREIGN KEY, not the reader. `creator.nil?`
/// would issue a SELECT on every validation (the reader is the
/// row-loading one), and the two agree by construction — the synthesized
/// writer stores 0 for nil, which is the same pair `inline_belongs_to_
/// check` tests in validations.rs.
///
/// The ASSIGNMENT goes through the writer rather than the ivar, because
/// the default lambda yields a RECORD (`Current.user`) and `creator=`
/// is what turns one into an id — including the nil case, which it
/// stores as 0 and the validation then rejects, exactly as Rails
/// rejects a default that evaluated to nil.
fn push_belongs_to_defaults(methods: &mut Vec<MethodDef>, model: &Model) {
    for assoc in model.associations() {
        let crate::dialect::Association::BelongsTo {
            name, foreign_key, default: Some(default), ..
        } = assoc
        else {
            continue;
        };
        let span = default.span;
        let fk = Expr::new(span, ExprNode::Ivar { name: foreign_key.clone() });
        let unset = Expr::new(
            span,
            ExprNode::BoolOp {
                op: crate::expr::BoolOpKind::Or,
                surface: crate::expr::BoolOpSurface::default(),
                left: Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(fk.clone()),
                        method: Symbol::from("nil?"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                ),
                right: Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(fk),
                        method: Symbol::from("=="),
                        args: vec![Expr::new(span, ExprNode::Lit { value: Literal::Int { value: 0 } })],
                        block: None,
                        parenthesized: false,
                    },
                ),
            },
        );
        let assign = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(Expr::new(span, ExprNode::SelfRef)),
                method: Symbol::from(format!("{}=", name.as_str())),
                args: vec![default.clone()],
                block: None,
                parenthesized: false,
            },
        );
        let stmt = Expr::new(
            span,
            ExprNode::If {
                cond: unset,
                then_branch: assign,
                else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            },
        );
        fold_into_or_push(methods, model, "before_validation", stmt);
    }
}

/// Per-model `cache_key` / `cache_key_with_version` — the identity a
/// fragment cache keys on.
///
///   def cache_key
///     return "messages/new" if new_record?
///     "messages/#{@id}"
///   end
///
///   def cache_key_with_version
///     "#{cache_key}-#{@updated_at_raw}"
///   end
///
/// THE TABLE NAME, NOT THE CLASS NAME, and that is a deliberate
/// divergence from Rails. Rails builds the prefix from `model_name
/// .cache_key`, so a `Rooms::Open` keys under `rooms/opens` while the
/// `Room` holding the same row keys under `rooms` — two entries for
/// one row. This emit hydrates an association's records as the BASE
/// class ([[project_campfire_compare_harness]], the STI dom_id
/// finding), so a class-derived prefix would make the key depend on
/// which read reached the row. The table name is the row's identity
/// and does not move.
///
/// THE VERSION IS `<col>_raw`, THE STORED TEXT, not Rails'
/// `updated_at.utc.to_fs(:usec)`. Both change exactly when the row's
/// timestamp changes, which is the whole contract; the raw text needs
/// no clock, no zone conversion and no strftime, and those are three
/// things that would each need an arm in nine emitters to agree on.
/// The key is never compared against a key Rails wrote — this store
/// is ours — so the FORMAT is free and only the invalidation matters.
///
/// A model with no `updated_at` column gets `cache_key_with_version ==
/// cache_key`, matching Rails, where `cache_version` is nil and the
/// suffix is dropped. Such a record can only be invalidated by its key
/// changing, which is Rails' exposure too.
pub(super) fn push_cache_key_methods(methods: &mut Vec<MethodDef>, model: &Model, schema: &Schema) {
    if is_primary_abstract_class(model) {
        return;
    }
    // NO TABLE, NO KEY — and for an STI subclass that is the point, not
    // a gap. `Rooms::Open`'s `table` is the class-derived `opens`, which
    // is in no schema; every schema-driven synthesizer here skips it for
    // the same reason and the subclass inherits the base's. Emitting one
    // anyway would key the SAME ROW under `opens/1` when a `Rooms::Open`
    // read reached it and `rooms/1` when a `Room` read did — the exact
    // two-entries-for-one-row split the table-name prefix exists to
    // avoid. A tableless model has no row to cache and falls out here
    // too.
    let Some(table) = schema.tables.get(&model.table.0) else { return };
    // A hand-written `cache_key` wins, as everywhere else here.
    if methods.iter().any(|m| {
        m.name.as_str() == "cache_key" && m.receiver == MethodReceiver::Instance
    }) {
        return;
    }
    let span = Span::synthetic();
    let table_name = model.table.0.as_str().to_string();

    // Rails' own answer for an unsaved record. Not a useful entry, but
    // a STABLE string, and the alternative is every unsaved record of a
    // class sharing the key `"<table>/0"` — a silent wrong-bytes
    // collision, which is the one failure mode this whole pass is
    // arranged to avoid.
    //
    // `self.persisted?`, NOT `new_record?`, and with an explicit
    // receiver. Only `persisted?` is synthesized per model on every
    // target (rust emits it as `self.id != 0`); `new_record?` lives on
    // the runtime Base, which a rust struct and a go struct do not
    // inherit — the first cut spelled it as a bare `new_record?` and
    // broke four CI jobs with `cannot find function new_record_pred`
    // and `undefined: NewRecord`. The explicit `self` is the spelling
    // `functionalize::mutation_to_struct_return` already uses for the
    // same call.
    //
    // An if-EXPRESSION rather than an early return: both arms are
    // String, so the strict targets get one typed value out of one
    // construct.
    let persisted = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(span, ExprNode::SelfRef)),
            method: Symbol::from("persisted?"),
            args: Vec::new(),
            block: None,
            parenthesized: false,
        },
    );
    let key_body = with_ty(
        Expr::new(
            span,
            ExprNode::StringInterp {
                parts: vec![
                    crate::expr::InterpPart::Text { value: format!("{table_name}/") },
                    crate::expr::InterpPart::Expr {
                        expr: Expr::new(span, ExprNode::Ivar { name: Symbol::from("id") }),
                    },
                ],
            },
        ),
        Ty::Str,
    );
    let unsaved = with_ty(
        Expr::new(
            span,
            ExprNode::Lit { value: Literal::Str { value: format!("{table_name}/new") } },
        ),
        Ty::Str,
    );
    let key = with_ty(
        Expr::new(
            span,
            ExprNode::If {
                cond: persisted.clone(),
                then_branch: key_body,
                else_branch: unsaved,
            },
        ),
        Ty::Str,
    );
    methods.push(str_method(model, "cache_key", key));

    // `updated_at` decides the version. Read through the `<col>_raw`
    // storage ivar (`col_storage_name`), which for a temporal column is
    // the ISO-8601 text as stored — the public reader would parse it
    // back into a Time only for us to format it again.
    let version_ivar = table
        .columns
        .iter()
        .find(|c| c.name.as_str() == "updated_at")
        .map(super::schema::col_storage_name);

    let versioned = match version_ivar {
        Some(ivar) => {
            // One interpolation, not `"#{cache_key}-#{raw}"`. The warm
            // collection-cache key walks every record; composing through
            // `cache_key` allocated a prefix string that was thrown away.
            let persisted_version = with_ty(
                Expr::new(
                    span,
                    ExprNode::StringInterp {
                        parts: vec![
                            crate::expr::InterpPart::Text {
                                value: format!("{table_name}/"),
                            },
                            crate::expr::InterpPart::Expr {
                                expr: Expr::new(span, ExprNode::Ivar { name: Symbol::from("id") }),
                            },
                            crate::expr::InterpPart::Text { value: "-".to_string() },
                            crate::expr::InterpPart::Expr {
                                expr: Expr::new(span, ExprNode::Ivar { name: ivar.clone() }),
                            },
                        ],
                    },
                ),
                Ty::Str,
            );
            let unsaved_version = with_ty(
                Expr::new(
                    span,
                    ExprNode::StringInterp {
                        parts: vec![
                            crate::expr::InterpPart::Text {
                                value: format!("{table_name}/new-"),
                            },
                            crate::expr::InterpPart::Expr {
                                expr: Expr::new(span, ExprNode::Ivar { name: ivar }),
                            },
                        ],
                    },
                ),
                Ty::Str,
            );
            with_ty(
                Expr::new(
                    span,
                    ExprNode::If {
                        cond: persisted,
                        then_branch: persisted_version,
                        else_branch: unsaved_version,
                    },
                ),
                Ty::Str,
            )
        }
        None => with_ty(
            Expr::new(
                span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("cache_key"),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            ),
            Ty::Str,
        ),
    };
    methods.push(str_method(model, "cache_key_with_version", versioned));
}

/// A no-arg instance method returning String — the shape both cache-key
/// methods share.
fn str_method(model: &Model, name: &str, body: Expr) -> MethodDef {
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body,
        signature: Some(fn_sig(vec![], Ty::Str)),
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// `belongs_to :message, touch: true` → the parent's `updated_at` is
/// stamped on four of this record's hooks:
///
///   def after_create
///     __touch_message = message
///     __touch_message.touch unless __touch_message.nil?
///   end
///
/// and the same in `after_update`, `after_destroy` and `after_touch`.
/// That is Rails' own registration set (`Builder::BelongsTo
/// .add_touch_callbacks`), and `after_touch` is the one that matters
/// most here: it is what makes the cascade transitive, so campfire's
/// Boost → Message → Room chain moves all three rows. `Base#touch`
/// fires it (runtime/ruby/active_record/base.rb).
///
/// THE LOCAL IS NOT A CONVENIENCE. The belongs_to reader is the
/// row-LOADING one and its signature is `Target | nil` whatever
/// `optional:` says, so a bare `message.touch unless message.nil?`
/// would issue the SELECT twice and read as a nilable receiver at the
/// call. Binding once and guarding the local is the shape
/// `attached.rs` already uses for the same reason.
///
/// Rails guards the update hook with `if: :saved_changes?` and skips
/// the touch when nothing actually changed. This runtime's `save`
/// issues an unconditional UPDATE, so there is no no-op save to skip
/// and the guard would never be false — omitted rather than emitted
/// as a constant true.
fn push_belongs_to_touches(methods: &mut Vec<MethodDef>, model: &Model) {
    for assoc in model.associations() {
        let crate::dialect::Association::BelongsTo { name, touch: Some(touch), .. } = assoc else {
            continue;
        };
        let span = Span::synthetic();
        let local = Symbol::from(format!("__touch_{}", name.as_str()));
        let read_local =
            || Expr::new(span, ExprNode::Var { id: VarId(0), name: local.clone() });

        let bind = Expr::new(
            span,
            ExprNode::Assign {
                target: LValue::Var { id: VarId(0), name: local.clone() },
                value: Expr::new(
                    span,
                    ExprNode::Send {
                        recv: None,
                        method: name.clone(),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                ),
            },
        );

        let mut guarded = Vec::new();
        // `touch: :last_message_at` stamps that column ALONGSIDE
        // `updated_at`. Written through the column WRITER and with
        // `ActiveSupport.db_now` for the same two reasons `column_ops`
        // spells it that way: the writer is what formats a temporal
        // value for storage, and `db_now` is the single clock the
        // `touch` below stamps `updated_at` from, so both columns
        // carry one instant and `travel_to` moves both.
        if let Touch::Column(col) = touch {
            guarded.push(Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(read_local()),
                    method: Symbol::from(format!("{}=", col.as_str())),
                    args: vec![Expr::new(
                        span,
                        ExprNode::Send {
                            recv: Some(Expr::new(
                                span,
                                ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] },
                            )),
                            method: Symbol::from("db_now"),
                            args: vec![],
                            block: None,
                            parenthesized: false,
                        },
                    )],
                    block: None,
                    parenthesized: false,
                },
            ));
        }
        // A stamped column has to reach the row with the timestamp, so
        // that form finishes with the whole-row `touch_written`; the bare
        // form's `touch` writes `updated_at` alone, as Rails' does.
        let finish = if matches!(touch, Touch::Column(_)) { "touch_written" } else { "touch" };
        guarded.push(Expr::new(
            span,
            ExprNode::Send {
                recv: Some(read_local()),
                method: Symbol::from(finish),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        ));

        let stmt = Expr::new(
            span,
            ExprNode::If {
                cond: Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(read_local()),
                        method: Symbol::from("nil?"),
                        args: vec![],
                        block: None,
                        parenthesized: false,
                    },
                ),
                then_branch: Expr::new(span, ExprNode::Lit { value: Literal::Nil }),
                else_branch: seq(guarded),
            },
        );

        for hook in ["after_create", "after_update", "after_destroy", "after_touch"] {
            fold_into_or_push(methods, model, hook, seq(vec![bind.clone(), stmt.clone()]));
        }
    }
}

/// Lifecycle hook names that appear as block-form Unknown items. Names
/// not in this set fall through to plain Unknown (they're future
/// lowerer or emit work). Includes the `_commit` variants Rails sugar
/// adds beyond the raw `after_commit` hook in `CallbackHook`, plus
/// `after_initialize` (fires on construction AND hydration — the
/// runtime hook call is appended by `synth_initialize` / the
/// hydration factories when a model declares it). The concern-items
/// ingest asks `block_callback_shape`, which reads this list.
pub(crate) const BLOCK_CALLBACK_HOOKS: &[&str] = &[
    "after_initialize",
    "before_validation",
    "after_validation",
    "before_save",
    "after_save",
    "before_create",
    "after_create",
    "before_update",
    "after_update",
    "before_destroy",
    "after_destroy",
    "after_commit",
    "after_rollback",
    "after_create_commit",
    "after_update_commit",
    "after_destroy_commit",
    "after_save_commit",
    "before_commit",
];

/// Lower lifecycle callbacks — both the symbol-form declarations
/// ingest recognized (`ModelBodyItem::Callback`, e.g. `before_save
/// :check_session_token`) and the block-form ones that surface as
/// Unknown items — into `def <hook>` overrides of the runtime Base's
/// no-op hooks. One body walk so declaration order is preserved
/// across both forms when they target the same hook.
///
/// `lifted` receives the methods a block callback's body becomes when
/// it cannot be folded into its hook (`push_block_callback`): they are
/// not hook overrides, so a subclass's hooks do not start them with
/// `super`.
pub(super) fn push_callback_methods(methods: &mut Vec<MethodDef>, lifted: &mut Vec<MethodDef>, model: &Model) {
    // AHEAD of the declared callbacks: Rails registers the
    // `belongs_to … default:` callback when the association is
    // declared, and campfire declares its associations above its
    // callbacks. A user callback that reads `creator` therefore sees
    // the default already applied, which is the order that matters.
    push_belongs_to_defaults(methods, model);
    push_belongs_to_touches(methods, model);
    for (index, item) in model.body.iter().enumerate() {
        match item {
            ModelBodyItem::Callback { callback, .. } => {
                push_symbol_callback(methods, model, callback, item.span());
            }
            ModelBodyItem::Unknown { expr, .. } => {
                match symbol_after_initialize(expr).filter(|_| !defines_after_initialize(model)) {
                    Some(body) => fold_into_or_push(methods, model, "after_initialize", body),
                    None => push_block_callback(methods, lifted, model, index, expr),
                }
            }
            _ => {}
        }
    }
}

/// `after_initialize :a, :b, if: :c` (or `unless:`): the self-calls the
/// hook runs, under their condition, in declaration order. A `prepend:`
/// changes nothing: ActiveModel sets it on every after-callback
/// (`_define_after_model_callback`). Rails has no `CallbackHook` for it (it fires on
/// construction and on load, not around a save), so it stays an
/// Unknown item and folds into the `after_initialize` override the
/// construction and hydration tails call (`schema::model_declares_after_initialize`).
/// None for any other shape (a block, a lambda, a non-symbol target or
/// option).
pub(crate) fn symbol_after_initialize(expr: &Expr) -> Option<Expr> {
    let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else { return None };
    if method.as_str() != "after_initialize" {
        return None;
    }
    let sym = |e: &Expr| match &*e.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
        _ => None,
    };
    let (opts, targets) = match args.split_last() {
        Some((last, rest)) if matches!(&*last.node, ExprNode::Hash { .. }) => (Some(last), rest),
        _ => (None, args.as_slice()),
    };
    let targets: Vec<Symbol> = targets.iter().map(sym).collect::<Option<_>>()?;
    if targets.is_empty() {
        return None;
    }
    let span = expr.span;
    let call = |name: Symbol| {
        Expr::new(span, ExprNode::Send { recv: None, method: name, args: vec![], block: None, parenthesized: false })
    };
    let mut condition = None;
    if let Some(opts) = opts {
        let ExprNode::Hash { entries, .. } = &*opts.node else { return None };
        for (key, value) in entries {
            match (sym(key)?.as_str(), &*value.node) {
                ("if", _) if condition.is_none() => condition = Some(call(sym(value)?)),
                ("unless", _) if condition.is_none() => {
                    condition = Some(Expr::new(
                        span,
                        ExprNode::Send {
                            recv: Some(call(sym(value)?)),
                            method: Symbol::from("!"),
                            args: vec![],
                            block: None,
                            parenthesized: false,
                        },
                    ))
                }
                ("prepend", ExprNode::Lit { value: Literal::Bool { .. } }) => {}
                _ => return None,
            }
        }
    }
    let mut body = seq(targets.into_iter().map(call).collect());
    if let Some(cond) = condition {
        body = Expr::new(
            span,
            ExprNode::If {
                cond,
                then_branch: body,
                else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            },
        );
    }
    Some(body)
}

/// A model that writes its own `def after_initialize` keeps it as
/// written: a symbol-form `after_initialize` beside it is not folded in.
pub(crate) fn defines_after_initialize(model: &Model) -> bool {
    model.body.iter().any(|item| {
        matches!(item, ModelBodyItem::Method { method, .. }
            if method.name.as_str() == "after_initialize" && method.receiver == MethodReceiver::Instance)
    })
}

/// Symbol-form callback → self-calls folded into the hook override.
/// `on:` restrictions lower structurally: `after_commit ..., on:
/// :create` targets the runtime's `after_create_commit` hook, and
/// validation hooks get a `new_record?` guard (accurate at validation
/// time — the insert hasn't happened yet). `if:`/`unless:` conditions
/// wrap the body in the guard they name. Ingest already rejected every
/// (hook, on) pair this match doesn't cover.
fn push_symbol_callback(
    methods: &mut Vec<MethodDef>,
    model: &Model,
    cb: &crate::dialect::Callback,
    span: Span,
) {
    use crate::dialect::CallbackHook as Hook;

    // The transaction callbacks build their chains in
    // `transaction_callbacks`.
    if cb.hook.is_transaction() {
        return;
    }
    let hook_name = hook_method_name(cb.hook);
    // `before_validation :generate_uuid` where `def generate_uuid; self.uuid
    // ||= SecureRandom.uuid; end`: the same `||=` the block form rewrites
    // (`push_block_callback`), over a method the callback names. Rails'
    // new-record string attribute is nil; this storage's is "".
    for target in &cb.targets {
        if let Some(def) = methods
            .iter_mut()
            .find(|m| m.name == *target && m.receiver == MethodReceiver::Instance)
        {
            rewrite_column_or_assign(&mut def.body, model);
            let span = def.body.span;
            def.body.inherit_span(span);
        }
    }
    let self_call = |name: &Symbol| {
        Expr::new(
            span,
            ExprNode::Send {
                recv: None,
                method: name.clone(),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        )
    };

    let mut body = seq(cb.targets.iter().map(self_call).collect());
    if matches!(cb.hook, Hook::BeforeValidation | Hook::AfterValidation) && cb.on.is_some() {
        // Validations never run on destroy; ingest rejects this.
        let Some(on) = cb.on else { return };
        let Some(guarded) = guard_validation_on(body, on, span) else { return };
        body = guarded;
    }
    // `if:` / `unless:` — the callback runs only when the condition
    // holds, exactly as Rails. The condition was already negated for
    // `unless:` at ingest.
    if let Some(cond) = &cb.condition {
        body = Expr::new(
            span,
            ExprNode::If {
                cond: cond.clone(),
                then_branch: body,
                else_branch: Expr::new(
                    Span::synthetic(),
                    ExprNode::Lit { value: Literal::Nil },
                ),
            },
        );
    }
    fold_into_or_push(methods, model, hook_name, body);
}

/// Wrap a validation-hook body in the `new_record?` guard its `on:`
/// restriction lowers to — accurate at validation time, since the
/// insert hasn't happened yet. `on: :destroy` has no reading here
/// (validations never run on destroy) and returns None: drop the
/// callback rather than run it in the wrong circumstances.
fn guard_validation_on(
    body: Expr,
    on: crate::dialect::CallbackOn,
    span: Span,
) -> Option<Expr> {
    use crate::dialect::CallbackOn as On;

    let new_record = Expr::new(
        span,
        ExprNode::Send {
            recv: None,
            method: Symbol::from("new_record?"),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    );
    let cond = match on {
        On::Create => new_record,
        On::Update => Expr::new(
            span,
            ExprNode::Send {
                recv: Some(new_record),
                method: Symbol::from("!"),
                args: vec![],
                block: None,
                parenthesized: false,
            },
        ),
        On::Destroy => return None,
    };
    let mut guarded = Expr::new(
        span,
        ExprNode::If {
            cond,
            then_branch: body,
            else_branch: Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        },
    );
    guarded.inherit_span(span);
    Some(guarded)
}

/// The runtime Base hook method a `CallbackHook` overrides when no
/// `on:` remap applies. Names match the no-op definitions in
/// `runtime/ruby/active_record/base.rb`.
fn hook_method_name(hook: crate::dialect::CallbackHook) -> &'static str {
    use crate::dialect::CallbackHook as Hook;
    match hook {
        Hook::BeforeValidation => "before_validation",
        Hook::AfterValidation => "after_validation",
        Hook::BeforeSave => "before_save",
        Hook::AfterSave => "after_save",
        Hook::BeforeCreate => "before_create",
        Hook::AfterCreate => "after_create",
        Hook::BeforeUpdate => "before_update",
        Hook::AfterUpdate => "after_update",
        Hook::BeforeDestroy => "before_destroy",
        Hook::AfterDestroy => "after_destroy",
        Hook::AfterCommit => "after_commit",
        Hook::AfterSaveCommit => "after_save_commit",
        Hook::AfterRollback => "after_rollback",
        Hook::BeforeCommit => "before_commit",
    }
}

/// The `on:` restriction from a block-form callback's option hash.
/// None means "a shape this lowering won't run": not an `on:` hash at
/// all, an unmodeled value, or extra options (`if:`/`unless:`) whose
/// conditions would be silently dropped.
fn block_callback_on(arg: &Expr) -> Option<crate::dialect::CallbackOn> {
    use crate::dialect::CallbackOn as On;

    let ExprNode::Hash { entries, .. } = &*arg.node else { return None };
    let [(k, v)] = &entries[..] else { return None };
    let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { return None };
    let ExprNode::Lit { value: Literal::Sym { value: val } } = &*v.node else { return None };
    if key.as_str() != "on" {
        return None;
    }
    match val.as_str() {
        "create" => Some(On::Create),
        "update" => Some(On::Update),
        "destroy" => Some(On::Destroy),
        // `on: [:create, :update]` array form: not modeled.
        _ => None,
    }
}

/// Rails means the same thing by both of these, and so does this —
///
/// ```text
/// after_create_commit { room.receive(self) }      # a BLOCK
/// after_create_commit -> { room.receive(self) }   # a lambda ARG
/// ```
///
/// — campfire's `Message` writes the second, and for three sessions
/// this function matched only the first, so `room.receive(self)` never
/// ran and every message's push was silently skipped.
///
/// THE FOUR LINES WERE NEVER THE PROBLEM. Folding the lambda form used
/// to take the campfire suite from 154 passing to ZERO: `receive`
/// reaches `push_later`, whose `Room::PushMessageJob.perform_later`
/// ran INLINE, so every message a FIXTURE loads ran
/// `Room::MessagePusher`, whose `Push::Subscription.joins(user:
/// :memberships)` is a nested join the association registry cannot
/// resolve and which RAISES rather than answer the wrong rows. Rails'
/// own tests never reach it because their adapter is `:test`, which
/// enqueues without running. That divergence landed first
/// (`lower::job_class_side`'s adapter gate + `ActiveJob::ENQUEUE_ONLY`),
/// and with it in place this widening is what it always looked like.
///
/// A lambda WITH PARAMETERS (`->(record) { … }`) still declines: Rails
/// `instance_exec`s the zero-arity form, so `self` is the record, but
/// passes the record as an ARGUMENT to the other and leaves `self` as
/// the declaring context. Two different bindings, and only one of them
/// is the shape this splices into a hook method body.
///
/// The body's own `next` ends the callback, as `return` ends a method
/// (`lower::lifted_block`). A body that returns early cannot share its
/// hook with the declarations folded beside it (a `return` would skip
/// theirs), so it becomes a method of its own, which the hook calls.
///
/// The block-form / lambda-argument callback `push_block_callback`
/// lowers, as (callback lambda, hook name, `on:` restriction), or `None`
/// when it declines the shape. `model_to_library`'s unlowered-DSL report
/// claims a callback exactly when this answers `Some`, so the two cannot
/// disagree about which callbacks become hooks. A claimed shape Rails
/// gives no meaning to (two positionals, a required keyword) is refused
/// by name in `push_block_callback`. The transaction hooks' block forms
/// are `transaction_callbacks`'.
pub(crate) fn block_callback_shape(
    expr: &Expr,
) -> Option<(&Expr, &str, Option<crate::dialect::CallbackOn>)> {
    let ExprNode::Send { recv: None, method, args, block, .. } = &*expr.node else {
        return None;
    };
    // The callback body is a BLOCK or a LAMBDA ARGUMENT (see the
    // doc comment). Either way what follows it is the option hash,
    // so both spellings share one `on:` parse below.
    let (callback, opt_args): (&Expr, &[Expr]) = match (block.as_ref(), &args[..]) {
        // A block's defaults bind ahead of the spliced body (Rails runs
        // the proc with `instance_exec` and at most the record), unless
        // one reads a local of the enclosing scope, which the hook has no
        // binding for. A `&method(:m)` block is no body to splice.
        (Some(b), rest) if block_defaults_bindable(b) => (b, rest),
        (Some(_), _) => return None,
        (None, [first, rest @ ..]) if matches!(
            &*first.node,
            ExprNode::Lambda { params, rest_param, signature, extra_params, block_param, .. }
                if params.is_empty() && rest_param.is_none() && signature.is_empty()
                    && extra_params.is_empty() && block_param.is_none()
        ) => {
            (first, rest)
        }
        _ => return None,
    };
    // `before_validation on: :create do … end` — the block form
    // carries its restriction as an option hash where the symbol
    // form carries it as a keyword. Anything else in that hash
    // (`if:`/`unless:`) declines the callback, matching ingest's
    // rejection for the symbol form.
    let on = match opt_args {
        [] => None,
        [opts] => Some(block_callback_on(opts)?),
        _ => return None,
    };
    let hook = method.as_str();
    if !BLOCK_CALLBACK_HOOKS.contains(&hook) || super::transaction_callbacks::is_transaction_hook(hook) {
        return None;
    }
    // Same structural lowering as the symbol form: after_commit
    // retargets the per-lifecycle hook, validation hooks keep
    // their name and gain a `new_record?` guard below. Rails
    // doesn't accept `on:` on the remaining hooks.
    let hook_name = match (hook, on) {
        (_, None) => hook,
        ("after_commit", Some(crate::dialect::CallbackOn::Create)) => "after_create_commit",
        ("after_commit", Some(crate::dialect::CallbackOn::Update)) => "after_update_commit",
        ("after_commit", Some(crate::dialect::CallbackOn::Destroy)) => "after_destroy_commit",
        ("before_validation" | "after_validation", Some(_)) => hook,
        _ => return None,
    };
    Some((callback, hook_name, on))
}

/// Can a block callback's defaults be bound ahead of its spliced body?
/// A default may read the record parameter or an earlier parameter
/// (`|a: 1, b: a|`), which the hook binds first; a local of the
/// enclosing scope (`|key: prefix|`) has no binding inside the hook.
/// `extra_params` is never filled by this ingest (the `signature` holds
/// them); a block carrying one is declined.
fn block_defaults_bindable(block: &Expr) -> bool {
    let ExprNode::Lambda { params, signature, extra_params, .. } = &*block.node else {
        return false;
    };
    if !extra_params.is_empty() {
        return false;
    }
    let mut bound: Vec<&Symbol> = params.iter().collect();
    for (name, default) in &signature.optionals {
        if !reads_only(default, &bound) {
            return false;
        }
        bound.push(name);
    }
    for (name, default) in &signature.keywords {
        if default.as_ref().is_some_and(|d| !reads_only(d, &bound)) {
            return false;
        }
        bound.push(name);
    }
    true
}

/// Does `expr` read no local but those in `bound`? (A nested lambda's
/// own parameters are locals it binds itself.)
fn reads_only(expr: &Expr, bound: &[&Symbol]) -> bool {
    match &*expr.node {
        ExprNode::Var { name, .. } => bound.contains(&name),
        ExprNode::Lambda { .. } => false,
        _ => {
            let mut ok = true;
            expr.node.for_each_child(&mut |c| ok = ok && reads_only(c, bound));
            ok
        }
    }
}

/// Every name a lambda binds itself: its positionals, rest, block
/// parameter and signature (and `extra_params`, on an IR that has them).
fn lambda_bound_names(node: &ExprNode) -> Vec<Symbol> {
    let ExprNode::Lambda { params, rest_param, extra_params, block_param, signature, .. } = node else {
        return Vec::new();
    };
    params
        .iter()
        .chain(rest_param.iter())
        .chain(extra_params.iter().map(|p| &p.name))
        .chain(block_param.iter())
        .chain(signature.names())
        .cloned()
        .collect()
}

fn collect_local_names(expr: &Expr, names: &mut std::collections::HashSet<Symbol>) {
    match &*expr.node {
        ExprNode::Var { name, .. } => {
            names.insert(name.clone());
        }
        ExprNode::Assign { target: LValue::Var { name, .. }, .. } => {
            names.insert(name.clone());
        }
        ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => {
            names.insert(name.clone());
        }
        ExprNode::MultiAssign { targets, .. } => {
            names.extend(targets.iter().filter_map(|target| match target {
                LValue::Var { name, .. } => Some(name.clone()),
                _ => None,
            }));
        }
        node @ ExprNode::Lambda { .. } => {
            names.extend(lambda_bound_names(node));
        }
        _ => {}
    }
    expr.node.for_each_child(&mut |child| collect_local_names(child, names));
}

fn fresh_callback_local(
    span: Span,
    index: usize,
    occupied: &mut std::collections::HashSet<Symbol>,
) -> Symbol {
    let base = format!("__roundhouse_callback_{}_{}", span.start, index);
    let mut suffix = 0;
    loop {
        let name = Symbol::from(format!("{base}_{suffix}"));
        if occupied.insert(name.clone()) {
            return name;
        }
        suffix += 1;
    }
}

/// Rename callback-local references while respecting nested lambda bindings.
/// Unshadowed nested lambdas deliberately follow the renamed outer binding so
/// each folded callback closure retains its own value.
fn rename_callback_locals(expr: &mut Expr, renames: &std::collections::HashMap<Symbol, Symbol>) {
    if matches!(&*expr.node, ExprNode::Lambda { .. }) {
        let shadowed = lambda_bound_names(&expr.node);
        let visible: std::collections::HashMap<Symbol, Symbol> = renames
            .iter()
            .filter(|(name, _)| !shadowed.contains(name))
            .map(|(name, replacement)| (name.clone(), replacement.clone()))
            .collect();
        expr.node.for_each_child_mut(&mut |child| rename_callback_locals(child, &visible));
        return;
    }
    match &mut *expr.node {
        ExprNode::Var { name, .. } => {
            if let Some(replacement) = renames.get(name) {
                *name = replacement.clone();
            }
        }
        ExprNode::Assign { target, .. } | ExprNode::OpAssign { target, .. } => {
            if let LValue::Var { name, .. } = target {
                if let Some(replacement) = renames.get(name) {
                    *name = replacement.clone();
                }
            }
            expr.node.for_each_child_mut(&mut |child| rename_callback_locals(child, renames));
        }
        ExprNode::MultiAssign { targets, .. } => {
            for target in targets {
                if let LValue::Var { name, .. } = target {
                    if let Some(replacement) = renames.get(name) {
                        *name = replacement.clone();
                    }
                }
            }
            expr.node.for_each_child_mut(&mut |child| rename_callback_locals(child, renames));
        }
        _ => expr.node.for_each_child_mut(&mut |child| rename_callback_locals(child, renames)),
    }
}

fn push_block_callback(
    methods: &mut Vec<MethodDef>,
    lifted: &mut Vec<MethodDef>,
    model: &Model,
    index: usize,
    expr: &Expr,
) {
    use crate::lower::lifted_block::{self, Form};
    {
        let Some((callback, hook_name, on)) = block_callback_shape(expr) else {
            return;
        };
        let ExprNode::Send { method, block, .. } = &*expr.node else {
            return;
        };
        let hook = method.as_str();
        let ExprNode::Lambda { body: lambda_body, signature, params, rest_param, block_param, .. } =
            &*callback.node
        else {
            return;
        };
        // Rails runs a one-argument callback proc as
        // `record.instance_exec(record, &blk)`: the record is both
        // `self` and the argument. Any other positional shape has no
        // Rails meaning (two parameters raise ArgumentError natively;
        // `|*args|` receives nothing), so it is refused by name.
        let bound_param = match (params.as_slice(), rest_param, block_param) {
            ([], None, None) => None,
            ([one], None, None) => Some(one.clone()),
            _ => {
                crate::emit::diagnostics::push(crate::diagnostic::Diagnostic::unsupported(
                    expr.span,
                    None,
                    "block_callback_params",
                    format!(
                        "`{hook} {{ |…| … }}` on `{}` takes parameters other than the single \
                         record argument Rails passes; the callback is not lowered",
                        model.name.0.as_str(),
                    ),
                ));
                return;
            }
        };
        // A block runs as a proc: its own `break`/`return` raise
        // LocalJumpError in Rails, which no hook method reproduces.
        let form = if block.is_some() { Form::Block } else { Form::Lambda };
        let jumps = lifted_block::jumps(lambda_body);
        if let Some(jump) = jumps.unliftable(form) {
            crate::emit::diagnostics::push(crate::diagnostic::Diagnostic::unsupported(
                expr.span,
                None,
                "block_callback_jump",
                format!(
                    "`{hook}` on `{}`: the callback's own `{jump}` has no hook-method equivalent \
                     (Rails raises LocalJumpError for a block's `break`/`return`); the callback is not lowered",
                    model.name.0.as_str(),
                ),
            ));
            return;
        }
        // Rails runs the proc with `instance_exec` and at most the record
        // (`CallTemplate.build`: arity 0 passes nothing, arity 1 the
        // record), so an optional positional or keyword takes its
        // default and a keyword rest is empty. A required keyword raises
        // ArgumentError on every save natively; it is refused by name,
        // since the diagnose pass claims every block-form hook and
        // returning quietly dropped the callback.
        let required: Vec<String> =
            signature.keywords.iter().filter(|(_, d)| d.is_none()).map(|(n, _)| format!("`{n}:`")).collect();
        if !required.is_empty() {
            crate::emit::diagnostics::push(crate::diagnostic::Diagnostic::unsupported(
                expr.span,
                None,
                "block_callback_signature",
                format!(
                    "`{hook} {{ |…| … }}` on `{}` takes the required keyword {}, which Rails' callback \
                     never passes (ArgumentError natively); the callback is not lowered",
                    model.name.0.as_str(),
                    required.join(", "),
                ),
            ));
            return;
        }

        // Each name the hook binds becomes a fresh local: folded beside
        // other callbacks in one hook, a `key` this block binds must not
        // become the `key` another block reads (a method call there, or
        // its own binding a nested lambda captured).
        let bound_names: Vec<Symbol> = bound_param.iter().chain(signature.names()).cloned().collect();
        let (bound_param, signature, lambda_body) = if bound_names.is_empty() {
            (bound_param, signature.clone(), lambda_body.clone())
        } else {
            let mut occupied = std::collections::HashSet::new();
            for method in methods.iter().chain(lifted.iter()) {
                occupied.extend(method.params.iter().map(|p| p.name.clone()));
                if let Some(param) = &method.block_param {
                    occupied.insert(param.name.clone());
                }
                collect_local_names(&method.body, &mut occupied);
            }
            collect_local_names(callback, &mut occupied);
            occupied.extend(bound_names.iter().cloned());
            let renames: std::collections::HashMap<Symbol, Symbol> = bound_names
                .iter()
                .enumerate()
                .map(|(i, name)| (name.clone(), fresh_callback_local(expr.span, i, &mut occupied)))
                .collect();
            let mut signature = signature.clone();
            for (name, _) in signature.optionals.iter_mut() {
                *name = renames[name].clone();
            }
            for (name, _) in signature.keywords.iter_mut() {
                *name = renames[name].clone();
            }
            if let Some(name) = signature.kwrest.as_mut().filter(|n| !n.as_str().is_empty()) {
                *name = renames[name].clone();
            }
            for default in signature.defaults_mut() {
                rename_callback_locals(default, &renames);
            }
            let mut body = lambda_body.clone();
            rename_callback_locals(&mut body, &renames);
            (bound_param.map(|name| renames[&name].clone()), signature, body)
        };
        let lambda_body = &lambda_body;

        // Translate Rails-API broadcast calls (`assoc.broadcast_replace_to(...)`
        // etc.) inside the block body to spinel-shape `Broadcasts.<action>(...)`
        // calls. Other content passes through unchanged.
        let assign = |name: Symbol, value: Expr| {
            Expr::new(
                Span::synthetic(),
                ExprNode::Assign { target: crate::expr::LValue::Var { id: crate::ident::VarId(0), name }, value },
            )
        };
        // `<param> = self`, then each default in source order, ahead of
        // the spliced body.
        let mut binds: Vec<Expr> = Vec::new();
        if let Some(name) = bound_param {
            binds.push(assign(name, Expr::new(Span::synthetic(), ExprNode::SelfRef)));
        }
        for (name, default) in &signature.optionals {
            binds.push(assign(name.clone(), default.clone()));
        }
        for (name, default) in &signature.keywords {
            binds.push(assign(name.clone(), default.clone().expect("required keywords refused above")));
        }
        if let Some(name) = signature.kwrest.as_ref().filter(|n| !n.as_str().is_empty()) {
            binds.push(assign(
                name.clone(),
                Expr::new(Span::synthetic(), ExprNode::Hash { entries: vec![], kwargs: false }),
            ));
        }
        let lambda_body = if binds.is_empty() {
            lambda_body.clone()
        } else {
            let mut stmts = binds;
            match &*lambda_body.node {
                ExprNode::Seq { exprs } => stmts.extend(exprs.clone()),
                _ => stmts.push(lambda_body.clone()),
            }
            seq(stmts)
        };
        let mut lambda_body = lambda_body;
        lifted_block::to_method_body(&mut lambda_body, form);
        let mut lambda_body = super::broadcasts::rewrite_rails_broadcast_calls(
            lambda_body,
            model,
        );
        // `self.<col> ||= v` on a string column → blank-guarded assign.
        // Rails' new-record attributes are nil, so `||=` means "set
        // unless set"; this runtime's storage defaults string slots to
        // "" (strict targets assign every field), which is truthy and
        // would starve the idiom (lobsters' Token concern generates
        // its unique token exactly this way in after_initialize).
        // Blank-on-"" IS the faithful reading of "unset" under the
        // ""-default storage model.
        rewrite_column_or_assign(&mut lambda_body, model);
        // The rewrite synthesizes wrapper nodes; whatever it left
        // span-less attributes to the hook declaration. Source subtrees
        // spliced through keep their exact spans.
        lambda_body.inherit_span(expr.span);

        if matches!(hook, "before_validation" | "after_validation") {
            if let Some(on) = on {
                let Some(guarded) = guard_validation_on(lambda_body, on, expr.span) else {
                    return;
                };
                lambda_body = guarded;
            }
        }

        // Its own method, called where the body would have been folded.
        if jumps.returns_early() {
            let name = lifted_callback_name(model, hook_name, index);
            lifted.push(hook_like_method(model, Symbol::from(name.as_str()), lambda_body));
            lambda_body = Expr::new(
                expr.span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from(name.as_str()),
                    args: Vec::new(),
                    block: None,
                    parenthesized: false,
                },
            );
        }

        let hook_sym = Symbol::from(hook_name);
        if let Some(existing) = methods.iter_mut().find(|m| m.name == hook_sym) {
            // Fold this block's body into the existing method, preserving
            // source order (existing body's stmts first, then this block's).
            let mut stmts = match &*existing.body.node {
                ExprNode::Seq { exprs } => exprs.clone(),
                _ => vec![existing.body.clone()],
            };
            match &*lambda_body.node {
                ExprNode::Seq { exprs } => stmts.extend(exprs.clone()),
                _ => stmts.push(lambda_body.clone()),
            }
            existing.body = seq(stmts);
        } else {
            methods.push(hook_like_method(model, hook_sym, lambda_body));
        }
    }
}

/// The method a block callback's body becomes when it returns early:
/// `_<model>_<hook>_callback_<body index>`, unique in the model and
/// distinct from an ancestor's (the model's name is in it).
fn lifted_callback_name(model: &Model, hook: &str, index: usize) -> String {
    let slug: String = crate::naming::snake_case(model.name.0.as_str())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("_{slug}_{hook}_callback_{index}")
}

/// A parameterless public instance method of `model`, typed from its
/// body, as the block-callback hooks are.
fn hook_like_method(model: &Model, name: Symbol, body: Expr) -> MethodDef {
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: crate::span::Span::synthetic(),
        name,
        receiver: MethodReceiver::Instance,
        params: Vec::new(),
        body,
        signature: None,
        effects: EffectSet::default(),
        enclosing_class: Some(model.name.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}
