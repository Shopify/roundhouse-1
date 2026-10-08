//! Modules a boot initializer mixes into a load hook's base:
//!
//! ```ruby
//! # Core's config/initializers/010_serialization_options.rb
//! ActiveSupport.on_load(:active_record) { include SerializationOptions }
//! ```
//!
//! Rails runs the block with `base.class_eval` when the hook fires
//! (activesupport lazy_load_hooks.rb). For `:active_record` that is
//! base.rb's `run_load_hooks(:active_record, ActiveRecord::Base)`, before
//! any model loads, so every model inherits the module above Base. The
//! emitted boot runs the block as written (`ingest::initializer_programs`
//! carries it and the boot fires the framework hook). This pass gives the
//! analyzer the same ancestry: the module joins the base's
//! `ClassInfo::includes` (`prepend` too: both put the module's methods
//! on the base's instances; `extend` joins `extends`), so a record's
//! `serializable_hash` or `cached_serialization_fields` resolves to the
//! module's method rather than to nothing. The app's root models
//! (`ApplicationRecord`) take Base's mixins as well, since the registry
//! ends a model's parent chain at its last app model.
//!
//! Read only where the block certainly runs the mixin: a statement of
//! the block (or of the begin the program wraps it in), with implicit or
//! `self` receiver, whose arguments are constants naming registered
//! modules as written. The block's constants read from the top-level
//! cref, so the written path is the name. A hook nothing fires, or one
//! whose base is `Object`, is not read.

use crate::app::App;
use crate::det::HashMap;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::ClassId;

use crate::analyze::ClassInfo;

pub(in crate::analyze) fn register(app: &App, classes: &mut HashMap<ClassId, ClassInfo>) {
    let holders: Vec<&ClassId> = app.initializer_programs.iter().map(|p| &p.holder).collect();
    let mut mixins: Vec<(ClassId, &'static str, ClassId)> = Vec::new();
    for class in app.library_classes.iter().filter(|lc| holders.contains(&&lc.name)) {
        for method in &class.methods {
            hook_blocks(&method.body, &mut |hook, block| {
                let Some(base) = crate::ingest::initializer_programs::hook_base(app, hook) else { return };
                if base == "Object" {
                    return;
                }
                statements(block, &mut |stmt| {
                    let ExprNode::Send { recv, method, args, block: None, .. } = &*stmt.node else { return };
                    if recv.as_ref().is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef)) {
                        return;
                    }
                    let kind = match method.as_str() {
                        "include" => "include",
                        "prepend" => "include",
                        "extend" => "extend",
                        _ => return,
                    };
                    let modules: Option<Vec<ClassId>> = args
                        .iter()
                        .map(|arg| match &*arg.node {
                            ExprNode::Const { path } => Some(crate::analyze::mixin_path_id(path)),
                            _ => None,
                        })
                        .collect();
                    let Some(modules) = modules.filter(|m| !m.is_empty()) else { return };
                    if !modules.iter().all(|m| classes.get(m).is_some_and(|info| info.is_module)) {
                        return;
                    }
                    for module in modules {
                        mixins.push((ClassId(crate::ident::Symbol::from(base.as_str())), kind, module));
                    }
                });
            });
        }
    }
    // The registry links a model only to an app model parent, so the app's
    // root models (`ApplicationRecord < ActiveRecord::Base`) end their
    // chain where Base would follow: Base's mixins join them too, after
    // their own, as Base sits above what they include.
    let base_id = ClassId(crate::ident::Symbol::from("ActiveRecord::Base"));
    let roots: Vec<ClassId> = app
        .models
        .iter()
        .filter(|m| m.parent.as_ref().is_some_and(|p| p.0.as_str().trim_start_matches("::") == base_id.0.as_str()))
        .filter(|m| classes.get(&m.name).is_some_and(|info| info.parent.is_none()))
        .map(|m| m.name.clone())
        .collect();
    for (base, kind, module) in mixins {
        let receivers: Vec<ClassId> =
            std::iter::once(base.clone()).chain(roots.iter().filter(|_| base == base_id).cloned()).collect();
        for receiver in receivers {
            let Some(info) = classes.get_mut(&receiver) else { continue };
            let list = if kind == "extend" { &mut info.extends } else { &mut info.includes };
            if !list.contains(&module) {
                list.push(module.clone());
            }
        }
    }
}

/// Each `ActiveSupport.on_load(:hook) { … }` in `expr`, with its block.
fn hook_blocks<'a>(expr: &'a Expr, f: &mut dyn FnMut(&str, &'a Expr)) {
    if let ExprNode::Send { recv: Some(recv), method, args, block: Some(block), .. } = &*expr.node {
        let on_active_support = matches!(&*recv.node, ExprNode::Const { path }
            if crate::analyze::mixin_path_id(path).0.as_str() == "ActiveSupport");
        if method.as_str() == "on_load" && on_active_support {
            if let Some(ExprNode::Lit { value: Literal::Sym { value } }) = args.first().map(|a| &*a.node) {
                if let ExprNode::Lambda { body, .. } = &*block.node {
                    f(value.as_str(), body);
                }
                return;
            }
        }
    }
    expr.node.for_each_child(&mut |child| hook_blocks(child, f));
}

/// The statements a block certainly runs: its sequence, and the body of
/// a `begin` (the initializer program wraps each statement in one).
fn statements<'a>(expr: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
    match &*expr.node {
        ExprNode::Seq { exprs } => exprs.iter().for_each(|e| statements(e, f)),
        ExprNode::BeginRescue { body, .. } => statements(body, f),
        _ => f(expr),
    }
}
