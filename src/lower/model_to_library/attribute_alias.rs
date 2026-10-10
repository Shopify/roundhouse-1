//! `alias_attribute :new, :old` over a column (`Model::attribute_aliases`,
//! Core's `alias_attribute :subdomain, :name`).
//!
//! Rails' `alias_attribute` defines the new name's attribute methods over
//! the column's (reader, writer, query and the Dirty spellings) and
//! resolves it wherever an attribute name is a key: `new`/`update`
//! hashes, `[]`/`[]=`, `where`, `order` and `pluck` (the relation asks
//! `Base._attribute_name`). Here each spelling the model compiles for the
//! column gets a delegating twin under the new name, the hash-taking
//! methods resolve alias keys first (`Base._resolve_attribute_aliases`),
//! `[]`/`[]=` gain the alias's arms, and the class answers its
//! `attribute_aliases`. The call itself runs at class load as written,
//! where the runtime's `alias_attribute` checks it names one of these.

use crate::dialect::{AccessorKind, MethodDef, MethodReceiver, Model};
use crate::expr::{Arm, Expr, ExprNode, LValue, Literal, Pattern};
use crate::ident::{ClassId, Symbol, VarId};
use crate::schema::{Column, Table};
use crate::span::Span;
use crate::ty::Ty;

use super::{fn_sig, lit_str, self_ref, var_ref, with_ty};

/// ActiveRecord's attribute method patterns (`prefix`, `suffix`): the
/// spellings `alias_attribute` defines for the new name, each over the
/// column's own.
const PATTERNS: &[(&str, &str)] = &[
    ("", ""),
    ("", "="),
    ("", "?"),
    ("", "_before_type_cast"),
    ("", "_for_database"),
    ("", "_came_from_user?"),
    ("", "_changed?"),
    ("", "_change"),
    ("", "_will_change!"),
    ("", "_was"),
    ("", "_previously_changed?"),
    ("", "_previous_change"),
    ("", "_previously_was"),
    ("restore_", "!"),
    ("clear_", "_change"),
    ("saved_change_to_", "?"),
    ("saved_change_to_", ""),
    ("", "_before_last_save"),
    ("will_save_change_to_", "?"),
    ("", "_change_to_be_saved"),
    ("", "_in_database"),
];

fn spelling(prefix: &str, name: &str, suffix: &str) -> Symbol {
    Symbol::from(format!("{prefix}{name}{suffix}"))
}

/// Whether `method`, a demand-gated spelling of `col`, is demanded under
/// an alias of the column: `subdomain_changed?` demands `name_changed?`.
pub(super) fn demanded_as_alias(
    model: &Model,
    col: &Column,
    method: &Symbol,
    demanded: &crate::det::HashSet<Symbol>,
) -> bool {
    model.attribute_aliases.iter().filter(|(_, old)| *old == col.name).any(|(new, old)| {
        PATTERNS.iter().any(|(p, s)| {
            spelling(p, old.as_str(), s) == *method && demanded.contains(&spelling(p, new.as_str(), s))
        })
    })
}

/// The alias spellings, the alias arms of `[]`/`[]=`, the alias keys of
/// `initialize`/`update`/`update!`, and `self.attribute_aliases`. Runs
/// after the schema methods are pushed; a model without aliases is left
/// as it was.
///
/// Rails' `define_attribute_methods` also runs `alias_attribute :id_value,
/// :id` on every model with an `id` column and no `id_value` one; that
/// alias gets its twins here too. A composite key's `id_value` is the
/// renamed column reader (`composite_key::rekey`), so it is skipped.
pub(super) fn push_alias_methods(methods: &mut Vec<MethodDef>, model: &Model, table: &Table) {
    let has_column = |name: &str| table.columns.iter().any(|c| c.name.as_str() == name);
    let implicit_id_value = has_column("id")
        && !has_column("id_value")
        && model.composite_primary_key.is_empty()
        && !model.attribute_aliases.iter().any(|(n, _)| n.as_str() == "id_value");
    if model.attribute_aliases.is_empty() && !implicit_id_value {
        return;
    }
    let mut aliases = model.attribute_aliases.clone();
    if implicit_id_value {
        aliases.push((Symbol::from("id_value"), Symbol::from("id")));
    }
    let owner = &model.name;
    let column_spellings: Vec<Symbol> = table
        .columns
        .iter()
        .flat_map(|c| [spelling("", c.name.as_str(), ""), spelling("", c.name.as_str(), "="), spelling("", c.name.as_str(), "?")])
        .collect();
    let mut twins: Vec<MethodDef> = Vec::new();
    for (new, old) in &aliases {
        for (p, s) in PATTERNS {
            let orig = spelling(p, old.as_str(), s);
            let twin = spelling(p, new.as_str(), s);
            // A spelling that is another column's own reader, writer or
            // query belongs to that column.
            if (p, s) != (&"", &"") && (p, s) != (&"", &"=") && (p, s) != (&"", &"?") && column_spellings.contains(&orig) {
                continue;
            }
            // The model's own `def` of the name wins, as over a column.
            if super::associations::model_defines_instance_method(model, &twin)
                || methods.iter().chain(twins.iter()).any(|m| m.receiver == MethodReceiver::Instance && m.name == twin)
            {
                continue;
            }
            let Some(target) = methods.iter().find(|m| m.receiver == MethodReceiver::Instance && m.name == orig) else {
                continue;
            };
            twins.push(delegate(target, twin));
        }
    }
    methods.extend(twins);
    // Without a written alias the runtime's `attribute_aliases` (the
    // `id_value` one) answers and the hash-taking methods resolve as is.
    if model.attribute_aliases.is_empty() {
        return;
    }
    for m in methods.iter_mut().filter(|m| m.receiver == MethodReceiver::Instance) {
        match m.name.as_str() {
            "[]" | "[]=" => add_alias_arms(&mut m.body, &model.attribute_aliases),
            "initialize" => resolve_attrs(&mut m.body, 1),
            "update" | "update!" => resolve_attrs(&mut m.body, 0),
            _ => {}
        }
    }
    methods.push(synth_attribute_aliases(owner, model, table));
}

/// `def <twin>(<params>); self.<target>(<params>); end`, typed as the target.
fn delegate(target: &MethodDef, twin: Symbol) -> MethodDef {
    let args: Vec<Expr> = target.params.iter().map(|p| var_ref(p.name.clone())).collect();
    let body = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(self_ref()),
            method: target.name.clone(),
            parenthesized: !args.is_empty(),
            args,
            block: None,
        },
    );
    MethodDef {
        name: twin,
        name_span: Span::synthetic(),
        body,
        kind: AccessorKind::Method,
        ..target.clone()
    }
}

/// The alias's arm beside its column's in the `case name` of `[]`/`[]=`.
fn add_alias_arms(body: &mut Expr, aliases: &[(Symbol, Symbol)]) {
    let mut done = false;
    find_name_case(body, &mut |arms| {
        if done {
            return;
        }
        done = true;
        let mut extra = Vec::new();
        for (new, old) in aliases {
            let Some(arm) = arms.iter().find(|a| matches!(&a.pattern, Pattern::Lit { value: Literal::Sym { value } } if value == old)) else {
                continue;
            };
            let mut twin = arm.clone();
            twin.pattern = Pattern::Lit { value: Literal::Sym { value: new.clone() } };
            extra.push(twin);
        }
        arms.extend(extra);
    });
}

fn find_name_case(e: &mut Expr, f: &mut dyn FnMut(&mut Vec<Arm>)) {
    match &mut *e.node {
        ExprNode::Case { scrutinee, arms } if matches!(&*scrutinee.node, ExprNode::Var { name, .. } if name.as_str() == "name") => f(arms),
        ExprNode::Seq { exprs } => exprs.iter_mut().for_each(|x| find_name_case(x, f)),
        _ => {}
    }
}

/// `attrs = self.class._resolve_attribute_aliases(attrs)` at statement
/// `at` of the body: the alias keys name their columns from there on.
fn resolve_attrs(body: &mut Expr, at: usize) {
    let attrs = Symbol::from("attrs");
    let self_class = Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: Some(self_ref()), method: Symbol::from("class"), args: Vec::new(), block: None, parenthesized: false },
    );
    let resolved = with_ty(
        Expr::new(
            Span::synthetic(),
            ExprNode::Send {
                recv: Some(self_class),
                method: Symbol::from("_resolve_attribute_aliases"),
                args: vec![var_ref(attrs.clone())],
                block: None,
                parenthesized: true,
            },
        ),
        Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Untyped) },
    );
    let assign = Expr::new(
        Span::synthetic(),
        ExprNode::Assign { target: LValue::Var { id: VarId(0), name: attrs }, value: resolved },
    );
    match &mut *body.node {
        ExprNode::Seq { exprs } => exprs.insert(at.min(exprs.len()), assign),
        _ => {
            let old = std::mem::replace(body, assign.clone());
            *body = super::seq(if at == 0 { vec![assign, old] } else { vec![old, assign] });
        }
    }
}

/// `def self.attribute_aliases; { "<new>" => "<old>", ..., "id_value" => "id" }; end`
/// — Rails' class_attribute, an ancestor's first, and the `id_value`
/// alias Rails gives a model with an `id` column, last.
fn synth_attribute_aliases(owner: &ClassId, model: &Model, table: &Table) -> MethodDef {
    let mut pairs: Vec<(String, String)> =
        model.attribute_aliases.iter().map(|(n, o)| (n.as_str().to_string(), o.as_str().to_string())).collect();
    if table.columns.iter().any(|c| c.name.as_str() == "id") && !pairs.iter().any(|(n, _)| n == "id_value") {
        pairs.push(("id_value".to_string(), "id".to_string()));
    }
    let entries = pairs.into_iter().map(|(n, o)| (lit_str(n), lit_str(o))).collect();
    let ty = Ty::Hash { key: Box::new(Ty::Str), value: Box::new(Ty::Str) };
    MethodDef {
        visibility: crate::dialect::MethodVisibility::Public,
        unsupported_formals: None,
        has_anonymous_block: false,
        name_span: Span::synthetic(),
        name: Symbol::from("attribute_aliases"),
        receiver: MethodReceiver::Class,
        params: Vec::new(),
        body: with_ty(Expr::new(Span::synthetic(), ExprNode::Hash { entries, kwargs: false }), ty.clone()),
        signature: Some(fn_sig(vec![], ty)),
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(owner.0.clone()),
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}
