//! A Concern's `included do class_attribute ... end` applied to a class
//! built at run time, `Class.new(Base) { include Concern ... }`.
//!
//! ```ruby
//! module ErrorCodes
//!   extend ActiveSupport::Concern
//!   included { class_attribute :auto_generated, default: false }
//! end
//!
//! Class.new(EnumType) do
//!   include ErrorCodes
//!   self.auto_generated = true
//! end
//! ```
//!
//! ActiveSupport runs the `included` block with the base `class_eval`ed
//! where the `include` is (`ActiveSupport::Concern#append_features`).
//! Ingest supplies a Concern's block to the named classes that include it
//! (`concern_accessors::splice_into_library_includers`) and the emitted
//! module keeps no hook, so an anonymous includer ran without the
//! attribute and `self.auto_generated = true` raised NoMethodError.
//!
//! Ingest has already expanded the block's `class_attribute`s into the
//! defs and seed assignments a named includer gets
//! (`class_attribute::methods`, `::seeds`). Those follow the `include`
//! inside the `Class.new` block, where Ruby runs them: a `def` there
//! defines on the anonymous class, and `@__class_attr_x = default` sets
//! on it. Only a block that is nothing but such defs and seeds is
//! carried; any other block keeps the Concern as it was.

use crate::App;
use crate::dialect::{MethodDef, ModelBodyItem};
use crate::expr::{Expr, ExprNode};
use crate::ident::ClassId;

use super::class_attribute;
use super::concern_class_attribute::const_path_to_class_id;

pub(super) fn expand(app: &mut App) {
    // The Concerns whose `included` block is defs and seeds only, each
    // with those statements.
    let carriers: crate::det::HashMap<ClassId, Vec<Expr>> = app
        .concern_model_items
        .iter()
        .filter_map(|(id, items)| {
            let statements: Option<Vec<Expr>> = items
                .iter()
                .map(|item| match item {
                    ModelBodyItem::Method { method, .. } => {
                        let mut def = method.clone();
                        def.enclosing_class = None;
                        Some(Expr::new(def.name_span, ExprNode::MethodDefExpr { def: Box::new(def) }))
                    }
                    ModelBodyItem::ClassBody { expr, .. } if class_attribute::seeded_name(expr).is_some() => Some(expr.clone()),
                    _ => None,
                })
                .collect();
            statements.filter(|statements| !statements.is_empty()).map(|statements| (id.clone(), statements))
        })
        .collect();
    if carriers.is_empty() {
        return;
    }
    let mut applied: Vec<ClassId> = Vec::new();
    let mut run = |scope: &str, methods: &mut [MethodDef]| {
        for method in methods {
            visit(&mut method.body, scope, &carriers, &mut applied);
        }
    };
    for lc in &mut app.library_classes {
        let scope = lc.name.0.as_str().to_string();
        run(&scope, &mut lc.methods);
    }
    for model in &mut app.models {
        let scope = model.name.0.as_str().to_string();
        let mut methods: Vec<&mut MethodDef> = model.methods_mut().collect();
        for method in &mut methods {
            visit(&mut method.body, &scope, &carriers, &mut applied);
        }
    }
    for id in applied {
        if !app.carried_included_blocks.contains(&id) {
            app.carried_included_blocks.push(id);
        }
    }
}

fn visit(expr: &mut Expr, scope: &str, carriers: &crate::det::HashMap<ClassId, Vec<Expr>>, applied: &mut Vec<ClassId>) {
    if let ExprNode::Send { recv: Some(recv), method, block: Some(block), .. } = &mut *expr.node {
        let class_new = method.as_str() == "new"
            && matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "Class");
        if class_new {
            if let ExprNode::Lambda { body, .. } = &mut *block.node {
                splice(body, scope, carriers, applied);
            }
        }
    }
    expr.node.for_each_child_mut(&mut |child| visit(child, scope, carriers, applied));
}

/// Each `include Concern` among the block's statements is followed by
/// the Concern's `class_attribute` statements.
fn splice(body: &mut Expr, scope: &str, carriers: &crate::det::HashMap<ClassId, Vec<Expr>>, applied: &mut Vec<ClassId>) {
    let statements: Vec<Expr> = match &*body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![body.clone()],
    };
    let mut out: Vec<Expr> = Vec::with_capacity(statements.len());
    let mut changed = false;
    for statement in statements {
        let concerns = included_concerns(&statement, scope, carriers);
        out.push(statement);
        for id in concerns {
            out.extend(carriers[&id].iter().cloned());
            if !applied.contains(&id) {
                applied.push(id);
            }
            changed = true;
        }
    }
    if changed {
        body.node = Box::new(ExprNode::Seq { exprs: out });
    }
}

/// The Concerns an `include A, B` statement names, resolved from the
/// lexical `scope` outward.
fn included_concerns(statement: &Expr, scope: &str, carriers: &crate::det::HashMap<ClassId, Vec<Expr>>) -> Vec<ClassId> {
    let ExprNode::Send { recv: None, method, args, .. } = &*statement.node else { return Vec::new() };
    if method.as_str() != "include" {
        return Vec::new();
    }
    args.iter()
        .filter_map(const_path_to_class_id)
        .filter_map(|written| {
            let written = written.0.as_str().trim_start_matches("::").to_string();
            let mut namespace = scope;
            loop {
                let candidate = if namespace.is_empty() { written.clone() } else { format!("{namespace}::{written}") };
                let id = ClassId(crate::ident::Symbol::from(candidate));
                if carriers.contains_key(&id) {
                    return Some(id);
                }
                match namespace.rfind("::") {
                    Some(at) => namespace = &namespace[..at],
                    None if !namespace.is_empty() => namespace = "",
                    None => return None,
                }
            }
        })
        .collect()
}
