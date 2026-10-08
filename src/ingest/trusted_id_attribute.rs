//! Core's `trusted_id_attribute :shop_id` (component gem,
//! `Component::TrustedIdAttributes`), lowered to what it defines.
//!
//! `ApplicationRecord` does `extend Component::TrustedIdAttributes`. The
//! module's `extended` hook runs
//! `model.class_attribute(:trusted_id_attributes, default: {},
//! instance_accessor: false)` and makes the writer private. Each
//! `trusted_id_attribute(attr_name, id_class = Component::TrustedId,
//! attribute: attr_name)` then
//!
//! - merges `attr_name => id_class` into that class attribute, and
//! - `class_eval`s a heredoc defining `trusted_<attr_name>`, which wraps
//!   the `attribute` reader's value in `id_class.new`, memoized in
//!   `@_trusted_<attr_name>` for as long as the id is unchanged.
//!
//! Both run here as Ruby defines them. The heredoc text is rebuilt with
//! the values Ruby interpolates: `#{id_class}` is the class's name,
//! written `::`-rooted so the emitted class body resolves the same class
//! that `class_eval` did. The class-attribute methods and seed come from
//! `ingest::class_attribute`, as for a written `class_attribute`.
//!
//! Applies only when the module and `Component::TrustedId` are app
//! source and the arguments are literal: a Symbol name, an `id_class`
//! constant that resolves to `Component::TrustedId` or a subclass (the
//! macro's own `TypeError` check), and a literal `attribute:`. The
//! `attribute: :id` form is left alone: its body branches on
//! `composite_primary_key?`, which this step does not evaluate. A call
//! left alone stays a dropped model-DSL statement.
//!
//! Interim: this step is keyed on the module and macro name, which a
//! lowering should not be. It stays as a measurement until generic model
//! macro replay lands (the model counterpart of
//! `inherited_class_macro_calls`, running the real macro against the
//! runtime). Replace it with that replay then; do not extend it.

use crate::det::HashSet;

use crate::App;
use crate::dialect::{MethodDef, MethodReceiver, MethodVisibility, ModelBodyItem};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;

const MODULE: &str = "Component::TrustedIdAttributes";
const TRUSTED_ID: &str = "Component::TrustedId";
const ATTR: &str = "trusted_id_attributes";

pub(crate) fn expand(app: &mut App) {
    let anchored = app.library_classes.iter().any(|c| {
        c.is_module
            && c.name.0.as_str() == MODULE
            && c.methods.iter().any(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == "trusted_id_attribute")
    });
    if !anchored || !app.library_classes.iter().any(|c| !c.is_module && c.name.0.as_str() == TRUSTED_ID) {
        return;
    }
    let known: HashSet<String> = app
        .library_classes
        .iter()
        .map(|c| c.name.0.as_str().to_string())
        .chain(app.models.iter().map(|m| m.name.0.as_str().to_string()))
        .collect();
    let extenders: HashSet<String> = app
        .models
        .iter()
        .filter(|m| m.body.iter().any(|item| is_extend_of_module(item, m.name.0.as_str(), &known)))
        .map(|m| m.name.0.as_str().to_string())
        .collect();
    if extenders.is_empty() {
        return;
    }
    let parents: Vec<(String, Option<String>)> = app
        .models
        .iter()
        .map(|m| {
            let parent = m.parent.as_ref().and_then(|p| resolve(m.name.0.as_str(), p.0.as_str(), &known));
            (m.name.0.as_str().to_string(), parent)
        })
        .collect();
    let class_parents: Vec<(String, Option<String>)> = app
        .library_classes
        .iter()
        .map(|c| {
            let parent = c.parent.as_ref().and_then(|p| resolve(c.name.0.as_str(), p.0.as_str(), &known));
            (c.name.0.as_str().to_string(), parent)
        })
        .collect();
    let parent_of = |name: &str| -> Option<String> {
        parents.iter().chain(class_parents.iter()).find(|(n, _)| n == name).and_then(|(_, p)| p.clone())
    };
    let descends = |name: &str, root: &dyn Fn(&str) -> bool| -> bool {
        let mut at = Some(name.to_string());
        for _ in 0..64 {
            let Some(n) = at else { return false };
            if root(&n) {
                return true;
            }
            at = parent_of(&n);
        }
        false
    };
    let is_trusted_id = |name: &str| descends(name, &|n: &str| n == TRUSTED_ID);

    for model in &mut app.models {
        let name = model.name.0.as_str().to_string();
        let extender = extenders.contains(&name);
        if !extender && !descends(&name, &|n: &str| extenders.contains(n)) {
            continue;
        }
        let owner = model.name.clone();
        let mut body = Vec::with_capacity(model.body.len());
        for item in std::mem::take(&mut model.body) {
            if extender && is_extend_of_module(&item, &name, &known) {
                body.push(item);
                body.extend(class_attribute_items(&owner));
                continue;
            }
            let ModelBodyItem::Unknown { expr, leading_comments, leading_blank_line } = &item else {
                body.push(item);
                continue;
            };
            let Some(call) = parse_call(expr, &name, &known).filter(|c| is_trusted_id(&c.id_class)) else {
                body.push(item);
                continue;
            };
            let Some((method, merge)) = lower_call(&owner, &call, expr.span) else {
                body.push(item);
                continue;
            };
            body.push(ModelBodyItem::Method {
                method,
                leading_comments: leading_comments.clone(),
                leading_blank_line: *leading_blank_line,
            });
            body.push(ModelBodyItem::ClassBody { expr: merge, leading_comments: Vec::new(), leading_blank_line: false });
        }
        model.body = body;
    }
}

struct Call {
    attr_name: String,
    id_class: String,
    attribute: String,
}

/// `extend Component::TrustedIdAttributes`, on the class itself.
fn is_extend_of_module(item: &ModelBodyItem, owner: &str, known: &HashSet<String>) -> bool {
    let ModelBodyItem::Unknown { expr, .. } = item else { return false };
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return false };
    if method.as_str() != "extend" || !on_self(recv) || args.len() != 1 {
        return false;
    }
    const_path(&args[0]).and_then(|p| resolve(owner, &p, known)).as_deref() == Some(MODULE)
}

fn on_self(recv: &Option<Expr>) -> bool {
    recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
}

/// A constant as written, `::`-prefixed when rooted.
fn const_path(expr: &Expr) -> Option<String> {
    let ExprNode::Const { path } = &*expr.node else { return None };
    let parts: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    let joined = parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("::");
    (!joined.is_empty()).then(|| if parts.first() == Some(&"") { format!("::{joined}") } else { joined })
}

/// `written` as Ruby resolves it from inside `owner`: the innermost
/// enclosing namespace that has it, outwards to the top level.
fn resolve(owner: &str, written: &str, known: &HashSet<String>) -> Option<String> {
    if let Some(rooted) = written.strip_prefix("::") {
        return known.contains(rooted).then(|| rooted.to_string());
    }
    let scope: Vec<&str> = owner.split("::").collect();
    (0..=scope.len()).rev().find_map(|n| {
        let candidate =
            if n == 0 { written.to_string() } else { format!("{}::{written}", scope[..n].join("::")) };
        known.contains(&candidate).then_some(candidate)
    })
}

fn sym(expr: &Expr) -> Option<String> {
    match &*expr.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        _ => None,
    }
}

fn is_method_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_lowercase())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn parse_call(expr: &Expr, owner: &str, known: &HashSet<String>) -> Option<Call> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return None };
    if method.as_str() != "trusted_id_attribute" || !on_self(recv) {
        return None;
    }
    let (positional, kwargs) = match args.split_last() {
        Some((last, rest)) if matches!(&*last.node, ExprNode::Hash { kwargs: true, .. }) => (rest, Some(last)),
        _ => (&args[..], None),
    };
    let attr_name = sym(positional.first()?)?;
    let id_class = match positional.get(1) {
        None => TRUSTED_ID.to_string(),
        Some(c) => resolve(owner, &const_path(c)?, known)?,
    };
    if positional.len() > 2 {
        return None;
    }
    let mut attribute = attr_name.clone();
    if let Some(kw) = kwargs {
        let ExprNode::Hash { entries, .. } = &*kw.node else { return None };
        for (k, v) in entries {
            if sym(k)?.as_str() != "attribute" {
                return None;
            }
            attribute = sym(v)?;
        }
    }
    (is_method_name(&attr_name) && is_method_name(&attribute) && attribute != "id").then_some(Call {
        attr_name,
        id_class,
        attribute,
    })
}

/// The heredoc's `def trusted_<attr_name>` (non-`id` branch) and the
/// class-attribute merge, as Ruby evaluates them.
fn lower_call(owner: &ClassId, call: &Call, span: Span) -> Option<(MethodDef, Expr)> {
    let Call { attr_name, id_class, attribute } = call;
    let ivar = format!("@_trusted_{attr_name}");
    let src = format!(
        "class RoundhouseTrustedIdAttribute\n  def trusted_{attr_name}\n    db_id = {attribute}\n    return nil unless db_id\n\n    return {ivar} if {ivar}&.db_id == db_id\n    {ivar} = ::{id_class}.new(db_id)\n  end\n\n  def self.roundhouse_merge\n    self.{ATTR} = {ATTR}.merge({attr_name}: ::{id_class})\n  end\nend\n"
    );
    let classes = match super::ingest_library_classes(src.as_bytes(), "<trusted_id_attribute>") {
        Ok(classes) => classes,
        Err(err) => {
            super::survey::record(&err);
            return None;
        }
    };
    let mut methods = classes.into_iter().flat_map(|c| c.methods);
    let mut method = methods.find(|m| m.receiver == MethodReceiver::Instance)?;
    let merge = methods.find(|m| m.receiver == MethodReceiver::Class)?.body;
    method.enclosing_class = Some(owner.0.clone());
    method.name_span = span;
    Some((method, merge))
}

/// What the `extended` hook adds: the class attribute's methods (the
/// writer private), then its `{}` seed.
fn class_attribute_items(owner: &ClassId) -> Vec<ModelBodyItem> {
    let span = Span::synthetic();
    let decl = super::class_attribute::Decl {
        names: vec![Symbol::from(ATTR)],
        instance_reader: false,
        instance_writer: false,
        instance_predicate: true,
        default: Some(Expr::new(span, ExprNode::Hash { entries: Vec::new(), kwargs: false })),
        span,
    };
    let mut out: Vec<ModelBodyItem> = super::class_attribute::methods(owner, &decl)
        .into_iter()
        .map(|mut method| {
            if method.receiver == MethodReceiver::Class && method.name.as_str() == format!("{ATTR}=") {
                method.visibility = MethodVisibility::Private;
            }
            ModelBodyItem::Method { method, leading_comments: Vec::new(), leading_blank_line: false }
        })
        .collect();
    out.extend(super::class_attribute::seeds(&decl).into_iter().map(|expr| ModelBodyItem::ClassBody {
        expr,
        leading_comments: Vec::new(),
        leading_blank_line: false,
    }));
    out
}
