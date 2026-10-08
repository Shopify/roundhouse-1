//! Rails' `ignored_columns` (model_schema.rb): the columns a model leaves
//! out of its attribute set. Rails builds the model's columns from the
//! table minus them, so it has no reader or writer for them, and its
//! SELECT, INSERT and UPDATE do not name them. Core writes the list in
//! the class body while a column waits to be dropped
//! (`self.ignored_columns += %w[legacy_flag]`).
//!
//! The list is read at compile time from the class-body writes of the
//! model and its model ancestors, in order: `self.ignored_columns =` and
//! `+=` of a literal Array of Strings or Symbols, or of another model's
//! list (`= ::Customer.ignored_columns`). Any other write leaves the list
//! unknown, and the write stays refused.
//!
//! When every model on a table knows its list, the columns all of them
//! ignore leave the table at the end of ingest
//! (`ingest::app`), so the attribute rows, the analyzer and the compiled
//! SQL see what Rails sees. The primary key never leaves. The writes of
//! a model none of whose ignored names is still a column then run at
//! class load as written, against the runtime's `ignored_columns=`.

use crate::App;
use crate::dialect::{Model, ModelBodyItem};
use crate::expr::{Expr, ExprNode, LValue, Literal, OpAssignOp};
use crate::span::Span;

/// The model's ignored columns (in write order, deduplicated) and the
/// spans of its own writes that set them; `None` when a write in the
/// model or a model ancestor is not one this module reads.
pub(crate) fn ignored_columns(model: &Model, app: &App) -> Option<(Vec<String>, Vec<Span>)> {
    let mut seen = Vec::new();
    resolve(model, app, &mut seen)
}

fn resolve<'a>(model: &'a Model, app: &'a App, seen: &mut Vec<&'a str>) -> Option<(Vec<String>, Vec<Span>)> {
    if seen.contains(&model.name.0.as_str()) {
        return None;
    }
    seen.push(model.name.0.as_str());
    let mut list = match model.parent.as_ref().and_then(|p| app.models.iter().find(|m| &m.name == p)) {
        Some(parent) => resolve(parent, app, seen)?.0,
        None => Vec::new(),
    };
    let mut spans = Vec::new();
    for item in &model.body {
        let (ModelBodyItem::Unknown { expr, .. } | ModelBodyItem::ClassBody { expr, .. }) = item else { continue };
        let (append, value) = match &*expr.node {
            ExprNode::Send { recv: Some(recv), method, args, block: None, .. }
                if writes_list(recv, method.as_str()) && method.as_str().ends_with('=') && args.len() == 1 =>
            {
                (false, &args[0])
            }
            ExprNode::Send { recv: Some(recv), method, .. } if writes_list(recv, method.as_str()) && method.as_str().ends_with('=') => {
                return None;
            }
            ExprNode::Assign { target: LValue::Attr { recv, name }, value } if writes_list(recv, name.as_str()) => (false, value),
            ExprNode::OpAssign { target: LValue::Attr { recv, name }, op: OpAssignOp::Add, value }
                if writes_list(recv, name.as_str()) =>
            {
                (true, value)
            }
            ExprNode::Assign { target: LValue::Attr { recv, name }, .. }
            | ExprNode::OpAssign { target: LValue::Attr { recv, name }, .. }
                if writes_list(recv, name.as_str()) =>
            {
                return None;
            }
            _ => continue,
        };
        let names = match literal_names(value) {
            Some(names) => names,
            None => {
                let other = other_models_list(value, model, app)?;
                resolve(other, app, seen)?.0
            }
        };
        if !append {
            list.clear();
        }
        for name in names {
            if !list.contains(&name) {
                list.push(name);
            }
        }
        spans.push(expr.span);
    }
    seen.pop();
    Some((list, spans))
}

fn writes_list(recv: &Expr, name: &str) -> bool {
    matches!(&*recv.node, ExprNode::SelfRef) && name.trim_end_matches('=') == "ignored_columns"
}

/// A literal Array of Strings or Symbols.
fn literal_names(value: &Expr) -> Option<Vec<String>> {
    let ExprNode::Array { elements, .. } = &*value.node else { return None };
    elements
        .iter()
        .map(|e| match &*e.node {
            ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
            _ => None,
        })
        .collect()
}

/// The model `Customer.ignored_columns` reads, resolved from `owner`'s
/// namespace outwards.
fn other_models_list<'a>(value: &Expr, owner: &Model, app: &'a App) -> Option<&'a Model> {
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*value.node else { return None };
    if method.as_str() != "ignored_columns" || !args.is_empty() {
        return None;
    }
    let ExprNode::Const { path } = &*recv.node else { return None };
    let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
    let written = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
    let find = |name: &str| app.models.iter().find(|m| m.name.0.as_str() == name);
    if rooted {
        return find(&written);
    }
    let scope: Vec<&str> = owner.name.0.as_str().split("::").collect();
    (0..scope.len()).rev().find_map(|n| if n == 0 { find(&written) } else { find(&format!("{}::{written}", scope[..n].join("::"))) })
}
