//! `Other.include(M)` in a class or module body, where both `Other` and
//! `M` are the app's own: activemerchant 1.137.0's billing/compatibility.rb
//! ends `module Billing` with
//!
//! ```ruby
//! Compatibility::Model.include Rails::Model
//! ```
//!
//! That gives `Compatibility::Model#valid?`'s `super` its method
//! (`Rails::Model#valid?`). A class-body call on another constant is
//! captured and then dropped by every emit that does not replay the
//! whole body, so `CreditCard#valid?` raised NoMethodError (no superclass
//! method). The call becomes an `include` of `Other` itself, which has
//! the same ancestors. Ruby 3+ also gives a module's existing includers
//! a module it includes later. `Other` and `M` are resolved lexically
//! from the body the call is written in, as Ruby resolves them there.
//! Anything else stays captured.
//!
//! A railtie writes the same call inside a boot block (activesupport's
//! lazy_load_hooks.rb `on_load`, railties' `initializer`, `config.to_prepare`):
//!
//! ```ruby
//! initializer "gem.setup" do
//!   Metadata.include RedactableModel
//! end
//! ```
//!
//! When `Other` is an ActiveSupport::Concern, that call adds `M` to its
//! `@_dependencies` (concern.rb `append_features`), so every class that
//! includes `Other` afterwards gets `M`'s `ClassMethods` and `included`
//! block. Ingest lowers the app's concerns into their includers, so the
//! dependency has to be an include of `Other` for the splices to see it.
//! The statement stays in its block: it still runs at boot and is
//! idempotent there.

use crate::App;
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

pub(super) fn carry(app: &mut App) {
    let mut moves: Vec<(usize, usize, ClassId, ClassId)> = Vec::new();
    // Includes in a boot block: the module is a dependency of the target
    // and the statement stays where it runs.
    let mut dependencies: Vec<(ClassId, ClassId)> = Vec::new();
    for (i, lc) in app.library_classes.iter().enumerate() {
        for (j, call) in lc.unknown_calls.iter().enumerate() {
            if let Some((target, module)) = include_on(call) {
                let Some(target) = resolve(lc, &target, None, &app.library_classes) else { continue };
                let Some(module) = resolve(lc, &module, Some(true), &app.library_classes) else { continue };
                moves.push((i, j, target.name.clone(), module.name.clone()));
                continue;
            }
            boot_block_statements(call, &mut |stmt| {
                let Some((target, module)) = include_on(stmt) else { return };
                let Some(target) = resolve(lc, &target, Some(true), &app.library_classes) else { return };
                let Some(module) = resolve(lc, &module, Some(true), &app.library_classes) else { return };
                dependencies.push((target.name.clone(), module.name.clone()));
            });
        }
    }
    for (i, j, target, module) in moves.into_iter().rev() {
        let Some(t) = app.library_classes.iter().position(|c| c.origin.is_none() && c.name == target) else {
            continue;
        };
        if !app.library_classes[t].includes.contains(&module) {
            app.library_classes[t].includes.push(module);
        }
        app.library_classes[i].unknown_calls.remove(j);
    }
    for (target, module) in dependencies {
        let Some(t) = app.library_classes.iter().position(|c| c.origin.is_none() && c.name == target) else {
            continue;
        };
        if target != module && !app.library_classes[t].includes.contains(&module) {
            app.library_classes[t].includes.push(module);
        }
    }
}

/// The statements a boot hook (`initializer "x" do … end`,
/// `config.to_prepare { … }`, `ActiveSupport.on_load(:x) { … }`) runs
/// unconditionally: its block's sequence, and the body of a `begin`,
/// through a hook the block itself registers (`on_load` in an
/// `initializer`).
fn boot_block_statements<'a>(call: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
    fn statements<'a>(expr: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
        match &*expr.node {
            ExprNode::Seq { exprs } => exprs.iter().for_each(|e| statements(e, f)),
            ExprNode::BeginRescue { body, .. } => statements(body, f),
            _ => {
                f(expr);
                boot_block_statements(expr, f);
            }
        }
    }
    let ExprNode::Send { method, block: Some(block), .. } = &*call.node else { return };
    if !matches!(
        method.as_str(),
        "initializer" | "to_prepare" | "before_initialize" | "after_initialize" | "before_eager_load" | "on_load"
    ) {
        return;
    }
    if let ExprNode::Lambda { body, .. } = &*block.node {
        statements(body, f);
    }
}

/// `A::B.include(C::D)`: the written receiver and module.
fn include_on(call: &Expr) -> Option<(String, String)> {
    let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*call.node else { return None };
    if method.as_str() != "include" {
        return None;
    }
    let [arg] = args.as_slice() else { return None };
    Some((written(recv)?, written(arg)?))
}

fn written(e: &Expr) -> Option<String> {
    let ExprNode::Const { path } = &*e.node else { return None };
    Some(path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::"))
}

/// `written` as Ruby resolves it from `lc`'s namespace outwards, among
/// the app's own (not reopened native) classes and modules; `module`
/// restricts the kind.
fn resolve<'a>(
    lc: &LibraryClass,
    written: &str,
    module: Option<bool>,
    classes: &'a [LibraryClass],
) -> Option<&'a LibraryClass> {
    let written = written.trim_start_matches("::");
    let scope: Vec<&str> = lc.name.0.as_str().split("::").collect();
    (0..=scope.len()).rev().find_map(|n| {
        let name = if n == 0 { written.to_string() } else { format!("{}::{written}", scope[..n].join("::")) };
        classes
            .iter()
            .find(|c| module.is_none_or(|m| c.is_module == m) && c.origin.is_none() && c.name.0.as_str() == name)
    })
}
