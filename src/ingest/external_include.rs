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

use crate::App;
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

pub(super) fn carry(app: &mut App) {
    let mut moves: Vec<(usize, usize, ClassId, ClassId)> = Vec::new();
    for (i, lc) in app.library_classes.iter().enumerate() {
        for (j, call) in lc.unknown_calls.iter().enumerate() {
            let Some((target, module)) = include_on(call) else { continue };
            let Some(target) = resolve(lc, &target, None, &app.library_classes) else { continue };
            let Some(module) = resolve(lc, &module, Some(true), &app.library_classes) else { continue };
            moves.push((i, j, target.name.clone(), module.name.clone()));
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
