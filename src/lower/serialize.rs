//! ActiveRecord `serialize :attr, coder: JSON` — schema-less JSON in a
//! column (text/string/json), decoded at the public accessor boundary
//! through [`JsonColumn`](crate) the same way a schema `t.json` column is.
//!
//! Claimed spellings: `coder: JSON`, positional `JSON`, and toplevel
//! `::JSON` (Const path `["", "JSON"]`). Bare YAML `serialize :prefs`,
//! custom coders, and Array/Hash positional classes stay unclaimed.

use std::collections::HashSet;

use crate::dialect::{Model, ModelBodyItem};
use crate::expr::{ExprNode, Literal, LValue};
use crate::ident::Symbol;
use crate::span::Span;

/// A `serialize` declaration this pass fully expands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerializeDecl {
    pub span: Span,
    pub column: Symbol,
}

/// Every claimed JSON `serialize` in a model body.
pub fn serialize_decls(model: &Model) -> Vec<SerializeDecl> {
    let json_shadowed = model.lexical_json_shadow
        || body_defines_json_const(&model.body, model.name.0.as_str());
    let mut out = Vec::new();
    for item in &model.body {
        let ModelBodyItem::Unknown { expr, .. } = item else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node else {
            continue;
        };
        if method.as_str() != "serialize" {
            continue;
        }
        let Some(column) = args.first().and_then(sym_lit) else { continue };
        if !is_json_coder_args(&args[1..], json_shadowed) {
            continue;
        }
        out.push(SerializeDecl {
            span: expr.span,
            column,
        });
    }
    out
}

/// Column names claimed by [`serialize_decls`].
pub fn json_serialize_columns(model: &Model) -> HashSet<Symbol> {
    serialize_decls(model).into_iter().map(|d| d.column).collect()
}

/// Whether a constant path written in `owner`'s body shadows bare `JSON`.
///
/// Bare `JSON`, owner-qualified `Owner::JSON` / `A::B::JSON` when `owner`
/// is `A::B`, and absolute `::JSON` all affect bare lookup. Unrelated
/// `Foo::JSON` does not.
pub(crate) fn const_path_shadows_bare_json(path: &[Symbol], owner: &str) -> bool {
    match path {
        [name] if name.as_str() == "JSON" => true,
        [root, name] if name.as_str() == "JSON" && root.as_str().is_empty() => true,
        _ => {
            let Some(last) = path.last() else { return false };
            if last.as_str() != "JSON" {
                return false;
            }
            let owner_parts: Vec<&str> = owner.split("::").filter(|p| !p.is_empty()).collect();
            path.len() == owner_parts.len() + 1
                && path[..owner_parts.len()]
                    .iter()
                    .zip(owner_parts.iter())
                    .all(|(seg, part)| seg.as_str() == *part)
        }
    }
}

/// `JSON = …` / `JSON ||= …` / nested `class JSON` / `module JSON` markers
/// in the model body shadow bare `JSON` / `coder: JSON`.
fn body_defines_json_const(body: &[ModelBodyItem], owner: &str) -> bool {
    body.iter().any(|item| {
        let ModelBodyItem::Unknown { expr, .. } = item else {
            return false;
        };
        match &*expr.node {
            ExprNode::Assign {
                target: LValue::Const { path },
                ..
            }
            | ExprNode::OpAssign {
                target: LValue::Const { path },
                ..
            } => const_path_shadows_bare_json(path, owner),
            _ => false,
        }
    })
}

fn is_json_coder_args(args: &[crate::expr::Expr], json_shadowed: bool) -> bool {
    match args {
        [only] => is_json_const(only, json_shadowed) || is_coder_json_hash(only, json_shadowed),
        _ => false,
    }
}

fn is_coder_json_hash(expr: &crate::expr::Expr, json_shadowed: bool) -> bool {
    let ExprNode::Hash { entries, .. } = &*expr.node else {
        return false;
    };
    if entries.len() != 1 {
        return false;
    }
    let (key, value) = &entries[0];
    matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "coder")
        && is_json_const(value, json_shadowed)
}

fn is_json_const(expr: &crate::expr::Expr, json_shadowed: bool) -> bool {
    let ExprNode::Const { path } = &*expr.node else {
        return false;
    };
    // Absolute `::JSON` always names the stdlib coder.
    if matches!(
        path.as_slice(),
        [root, name] if root.as_str().is_empty() && name.as_str() == "JSON"
    ) {
        return true;
    }
    // Bare `JSON` only when the model body does not define a shadowing JSON.
    !json_shadowed && matches!(path.as_slice(), [name] if name.as_str() == "JSON")
}

fn sym_lit(expr: &crate::expr::Expr) -> Option<Symbol> {
    match &*expr.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.clone()),
        _ => None,
    }
}
