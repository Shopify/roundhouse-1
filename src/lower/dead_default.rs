//! Dead kwarg-default grounding: a param default that is a bare
//! zero-arg send resolving to NOTHING on the defining class rewrites
//! to `raise "undefined local variable or method '<name>'"`.
//!
//! MRI never evaluates a default the caller supplies, so Rails code
//! carries statically-broken defaults (lobsters' `for_user: user` on
//! `User#recent_threads` — no `user` method anywhere on the chain,
//! every call site passes the kwarg). AOT targets compile the default
//! expression regardless and refuse the unresolvable send. The raise
//! preserves MRI's observable behavior — evaluating the default
//! raises with the same message — while being statically
//! compilable.
//!
//! Conservative on both sides: only fires when the defining class IS
//! in the analyzer registry and the name resolves nowhere on the
//! chain (own methods, attribute row, includes, then parents — the
//! same order dispatch uses). A class the registry doesn't know, or
//! a name any hop knows, keeps its default untouched. A class method's
//! default resolves on the class side (its own and its parents' class
//! methods, then the instance methods of the modules it extends):
//! Core's `def apply!(reader = query_engine_reader)` in `class << self`
//! names the private class method below it, and was grounded to the
//! raise because no instance method had that name. The analyzer
//! already ledgers these sites (`unresolved_type` on reads of the
//! poisoned param), so no new diagnostic is added here.

use crate::det::HashMap;

use crate::analyze::ClassInfo;
use crate::app::App;
use crate::dialect::{MethodReceiver, Param};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::ClassId;

pub fn apply_dead_default_lowering(app: &mut App, registry: &HashMap<ClassId, ClassInfo>) {
    for model in &mut app.models {
        let class = model.name.clone();
        for item in &mut model.body {
            if let crate::dialect::ModelBodyItem::Method { method, .. } = item {
                ground_params(&mut method.params, &class, method.receiver, registry);
            }
        }
    }
    for lc in &mut app.library_classes {
        let class = lc.name.clone();
        for method in &mut lc.methods {
            ground_params(&mut method.params, &class, method.receiver, registry);
        }
    }
}

fn ground_params(
    params: &mut [Param],
    class: &ClassId,
    receiver: MethodReceiver,
    registry: &HashMap<ClassId, ClassInfo>,
) {
    if !registry.contains_key(class) {
        return;
    }
    for p in params {
        let Some(default) = &mut p.default else { continue };
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &*default.node else {
            continue;
        };
        let known = match receiver {
            MethodReceiver::Instance => resolves(class, method.as_str(), false, registry, 0),
            MethodReceiver::Class => resolves_on_class(class, method.as_str(), registry, 0),
        };
        if !args.is_empty() || known {
            continue;
        }
        let span = default.span;
        *default = Expr::new(
            span,
            ExprNode::Raise {
                value: Expr::new(
                    span,
                    ExprNode::Lit {
                        value: Literal::Str {
                            value: format!(
                                "undefined local variable or method '{}'",
                                method.as_str()
                            ),
                        },
                    },
                ),
            },
        );
    }
}

/// A bare name in a default is read on `self`: the instance for an
/// instance method, the class itself for a class method (`def
/// open(path = default_path)` under `class << self` calls the class's
/// own `default_path`, which this pass used to turn into a raise
/// because it looked among instance methods only).
fn resolves(
    class: &ClassId,
    name: &str,
    class_side: bool,
    registry: &HashMap<ClassId, ClassInfo>,
    depth: usize,
) -> bool {
    // Unknown hop = can't prove absence; treat as resolving. Depth cap
    // guards a cyclic parent chain.
    if depth > 8 {
        return true;
    }
    let Some(info) = registry.get(class) else { return true };
    let own = if class_side {
        info.class_methods.keys().any(|m| m.as_str() == name)
    } else {
        info.instance_methods.keys().any(|m| m.as_str() == name)
            || info.attributes.fields.keys().any(|f| f.as_str() == name)
    };
    if own {
        return true;
    }
    if info.includes.iter().any(|m| resolves(m, name, class_side, registry, depth + 1)) {
        return true;
    }
    match &info.parent {
        Some(parent) => resolves(parent, name, class_side, registry, depth + 1),
        None => false,
    }
}

/// [`resolves`] for a class method's default: the class's own class
/// methods, the instance methods of the modules it extends, then its
/// parent's class side. A name every class answers (Module's and
/// Kernel's own, `name`, `superclass`) resolves without a registry entry.
fn resolves_on_class(
    class: &ClassId,
    name: &str,
    registry: &HashMap<ClassId, ClassInfo>,
    depth: usize,
) -> bool {
    if depth > 8 {
        return true;
    }
    const MODULE_METHODS: &[&str] = &[
        "name", "to_s", "inspect", "superclass", "ancestors", "class", "hash", "object_id",
        "itself", "frozen?", "freeze", "dup", "clone", "new", "allocate", "instance_methods",
        "constants", "module_parent", "module_parent_name", "caller", "binding", "block_given?",
        "__method__", "rand", "logger",
    ];
    if MODULE_METHODS.contains(&name) {
        return true;
    }
    let Some(info) = registry.get(class) else { return true };
    if info.class_methods.keys().any(|m| m.as_str() == name) {
        return true;
    }
    if info.extends.iter().any(|m| resolves(m, name, false, registry, depth + 1)) {
        return true;
    }
    match &info.parent {
        Some(parent) => resolves_on_class(parent, name, registry, depth + 1),
        None => false,
    }
}
